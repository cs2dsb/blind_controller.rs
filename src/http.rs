use core::{fmt::Debug, str::Utf8Error};

use embassy_net::{dns::DnsSocket, tcp::client::{TcpClient, TcpClientState}, Stack};
use esp_hal::rng::Rng;
use heapless::{String, Vec};
use log::{debug, error, trace};
use rand_core::RngCore;
use reqwless::{client::{HttpClient, TlsConfig, TlsVerify}, request::Method, response::StatusCode};
use embedded_io_async::BufRead;
use embassy_sync::mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use crate::rng::RngWrapper;

struct Buffers {
    response_buffer: [u8; HTTP_BUFFER_MAX_SIZE],
    tcp_client_state: TcpClientState<1, 4096, 4096>,
    read_record_buffer: [u8; TCP_MAX_RECORD_SIZE],
    write_record_buffer: [u8; TCP_MAX_RECORD_SIZE],
}

impl Buffers {
    const fn new() -> Self {
        Self {
            response_buffer: [0; HTTP_BUFFER_MAX_SIZE],
            tcp_client_state: TcpClientState::new(),
            read_record_buffer: [0; TCP_MAX_RECORD_SIZE],
            write_record_buffer: [0; TCP_MAX_RECORD_SIZE],
        }
    }
}

static BUFFERS: Mutex<CriticalSectionRawMutex, Buffers> = Mutex::new(Buffers::new());

#[derive(Debug)]
pub enum Error {
    Reqwless(reqwless::Error),
    ResponseTooLarge,
    RequestMissingLocation,
    UrlTooLong,
    Utf8Error(Utf8Error),
    BadStatus(StatusCode),
}

impl From<reqwless::Error> for Error {
    fn from(value: reqwless::Error) -> Self {
        Self::Reqwless(value)
    }
}

impl From<Utf8Error> for Error {
    fn from(value: Utf8Error) -> Self {
        Self::Utf8Error(value)
    }
}

#[derive(Debug)]
pub enum CallbackError<E> {
    Outer(Error),
    Callback(E)
}

impl<E> From<Error> for CallbackError<E> {
    fn from(value: Error) -> Self {
        Self::Outer(value)
    }
}

impl<E> From<reqwless::Error> for CallbackError<E> {
    fn from(value: reqwless::Error) -> Self {
        Self::Outer(Error::Reqwless(value))
    }
}

impl<E> From<Utf8Error> for CallbackError<E> {
    fn from(value: Utf8Error) -> Self {
        Self::Outer(Error::Utf8Error(value))
    }
}

const TCP_MAX_RECORD_SIZE: usize = 16640; // 16640 is the max TLS record size
const HTTP_BUFFER_MAX_SIZE: usize = 16384; 
const HEADER_LOCATION: &str = "location";
const URL_MAX_LENGTH: usize = 2048;

pub struct Client<'a> {
    stack: Stack<'a>,
    rng: RngWrapper,
}

fn to_url_string<T: AsRef<[u8]>>(input: T) -> Result<String<URL_MAX_LENGTH>, Error> {
    let url_bytes = Vec::<u8, URL_MAX_LENGTH>::from_slice(input.as_ref()).map_err(|()| Error::UrlTooLong)?;
    let url = String::<URL_MAX_LENGTH>::from_utf8(url_bytes)?;
    Ok(url)
}

impl<'a> Client<'a> {
    pub fn new(stack: Stack<'a>, rng: Rng) -> Client<'a> {
        let rng = RngWrapper::from(rng);
        
        Self {
            stack,
            rng,
        }
    }

    pub async fn req<const MAX_RESP_LEN: usize, T: AsRef<str>>(&mut self, url: T) -> Result<Vec<u8, MAX_RESP_LEN>, Error> {
        let url = url.as_ref();
        let mut url = to_url_string(url)?;
    
        debug!("Create DNS socket");
        let dns_socket = DnsSocket::new(self.stack);

        // Too big for the stack
        let buffers = &mut *BUFFERS.lock().await;
    
        let seed = self.rng.next_u64();
        let tls_config = TlsConfig::new(
            seed,
            &mut buffers.read_record_buffer,
            &mut buffers.write_record_buffer,
            TlsVerify::None,
        );

        debug!("Create TCP client");
        let tcp_client = TcpClient::new(self.stack, &buffers.tcp_client_state);

        debug!("Create HTTP client");
        let mut client = HttpClient::new_with_tls(&tcp_client, &dns_socket, tls_config);

        let buf = &mut buffers.response_buffer;
        
        loop {
            debug!("Send HTTP request to {url:?}");
            
            url = {
                debug!("Create HTTP request");
                let mut req = client.request(Method::GET, &url).await?;
                let resp = req.send(buf).await?;

                let status = resp.status;
                debug!("Response status: {:?}", status);

                if status.is_successful() {
                    let body = resp.body().read_to_end().await?;
                    debug!("Read {} bytes", body.len());
                    let output = Vec::<u8, MAX_RESP_LEN>::from_slice(body).map_err(|()| Error::ResponseTooLarge)?;
                    return Ok(output);
                }


                if !status.is_redirection() {
                    return Err(Error::BadStatus(status));
                }

                if let Some((_, redirect)) = resp.headers().find(|(name, _)| {
                    // debug!("Header: {name}");
                    name.eq_ignore_ascii_case(HEADER_LOCATION)
                }
                ) {
                    let redirect = to_url_string(redirect)?;
                    debug!("Redirecting");
                    redirect
                } else {
                    error!("Redirect ({:?}) without a location header!", resp.status);
                    return Err(Error::RequestMissingLocation);
                }
            };
        }
    }

    pub async fn req_buffered<T, F, E>(&mut self, url: T, mut f: F) -> Result<(), CallbackError<E>> 
    where 
        T: AsRef<str>,
        F: FnMut(&[u8]) -> Result<(), E>,
        E: Debug
    {
        let url = url.as_ref();
        let mut url = to_url_string(url)?;
    
        debug!("Create DNS socket");
        let dns_socket = DnsSocket::new(self.stack);
   
        // Too big for the stack
        let buffers = &mut *BUFFERS.lock().await;
        
        let seed = self.rng.next_u64();
        let tls_config = TlsConfig::new(
            seed,
            &mut buffers.read_record_buffer,
            &mut buffers.write_record_buffer,
            TlsVerify::None,
        );


        debug!("Create TCP client");
        let tcp_client = TcpClient::new(self.stack, &buffers.tcp_client_state);

        debug!("Create HTTP client");
        let mut client = HttpClient::new_with_tls(&tcp_client, &dns_socket, tls_config);

        let buf = &mut buffers.response_buffer;
        
        loop {
            debug!("Send HTTP request to {url:?}");
            
            url = {
                debug!("Create HTTP request");
                let mut req = client.request(Method::GET, &url).await?;
                let resp = req.send(buf).await?;

                let status = resp.status;
                debug!("Response status: {:?}", status);

                if status.is_successful() {
                    let content_length = resp.content_length.unwrap_or(20*1024*1024);
                    let mut tot = 0;
                    let body = resp.body();
                    let mut reader = body.reader();
                    let mut last_print = 0;
                    loop {
                        let n = {
                            // trace!("Fill buffer");
                            let buf = reader.fill_buf().await?;
                            if buf.len() == 0 {
                                break;
                            }

                            tot += buf.len();
                            
                            if tot >= last_print + 10 * 1024 {
                                last_print = tot;
                                trace!("Got {} {} / {}", buf.len(), tot, content_length);
                            }
                            if tot > content_length {
                                panic!("downloaded more than the content length... {content_length}");
                            }

                            if let Err(e) = f(buf) {
                                Err(CallbackError::Callback(e))?
                            }
                            buf.len()
                        };

                        reader.consume(n);
                    }
                    
                    return Ok(());
                }


                if !status.is_redirection() {
                    Err(Error::BadStatus(status))?;
                }

                if let Some((_, redirect)) = resp.headers().find(|(name, _)| {
                    // debug!("Header: {name}");
                    name.eq_ignore_ascii_case(HEADER_LOCATION)
                }
                ) {
                    let redirect = to_url_string(redirect)?;
                    debug!("Redirecting");
                    redirect
                } else {
                    error!("Redirect ({:?}) without a location header!", resp.status);
                    Err(Error::RequestMissingLocation)?
                }
            };
        }
    }
}