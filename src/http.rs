use core::str::Utf8Error;

use embassy_net::{dns::DnsSocket, tcp::client::{TcpClient, TcpClientState}, Stack};
use esp_hal::rng::Rng;
use heapless::{String, Vec};
use log::{debug, error};
use rand_core::RngCore;
use reqwless::{client::{HttpClient, TlsConfig, TlsVerify}, request::Method, response::StatusCode};

use crate::rng::RngWrapper;

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

const TCP_MAX_RECORD_SIZE: usize = 16384;
const HTTP_BUFFER_MAX_SIZE: usize = 10000; //4096;
const HEADER_LOCATION: &str = "location";
const URL_MAX_LENGTH: usize = 2048;

pub struct Client<'a> {
    stack: Stack<'a>,
    rng: RngWrapper,
    tcp_client_state: TcpClientState<1, 4096, 4096>,
    read_record_buffer: [u8; TCP_MAX_RECORD_SIZE],
    write_record_buffer: [u8; TCP_MAX_RECORD_SIZE],
}

fn to_url_string<T: AsRef<[u8]>>(input: T) -> Result<String<URL_MAX_LENGTH>, Error> {
    let url_bytes = Vec::<u8, URL_MAX_LENGTH>::from_slice(input.as_ref()).map_err(|()| Error::UrlTooLong)?;
    let url = String::<URL_MAX_LENGTH>::from_utf8(url_bytes)?;
    Ok(url)
}

impl<'a> Client<'a> {
    pub fn new(stack: Stack<'a>, rng: Rng) -> Client<'a> {
        debug!("Create TCP client state");
        let tcp_client_state = TcpClientState::new();
        
        let rng = RngWrapper::from(rng);
        let read_record_buffer = [0; TCP_MAX_RECORD_SIZE];
        let write_record_buffer = [0; TCP_MAX_RECORD_SIZE];
        
        Self {
            stack,
            rng,
            tcp_client_state,
            read_record_buffer,
            write_record_buffer,
        }
    }

    pub async fn req<const MAX_RESP_LEN: usize, T: AsRef<str>>(&mut self, url: T) -> Result<Vec<u8, MAX_RESP_LEN>, Error> {
        let url = url.as_ref();
        let mut url = to_url_string(url)?;
    
        debug!("Create DNS socket");
        let dns_socket = DnsSocket::new(self.stack);
    
        let seed = self.rng.next_u64();
        let tls_config = TlsConfig::new(
            seed,
            &mut self.read_record_buffer,
            &mut self.write_record_buffer,
            TlsVerify::None,
        );

        debug!("Create TCP client");
        let tcp_client = TcpClient::new(self.stack, &self.tcp_client_state);

        debug!("Create HTTP client");
        let mut client = HttpClient::new_with_tls(&tcp_client, &dns_socket, tls_config);

        // Too big for the stack
        let buf = mk_static!([u8; HTTP_BUFFER_MAX_SIZE], [0; HTTP_BUFFER_MAX_SIZE]);
        
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
                    debug!("Header: {name}");
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
}