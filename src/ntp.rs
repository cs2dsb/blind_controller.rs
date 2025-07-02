use embassy_net::dns::DnsSocket;
use embassy_net::dns::Error as DnsError;
use embassy_net::udp::BindError;
use embassy_net::udp::PacketMetadata;
use embassy_net::udp::RecvError;
use embassy_net::udp::SendError;
use embassy_net::udp::UdpSocket;
use embassy_net::IpAddress;
use embassy_net::dns::DnsQueryType;


use core::net::Ipv4Addr;
use core::net::SocketAddrV4;

const NTP_PORT: u16 = 123;

use log::*;

use sntpc::get_time;
use sntpc::fraction_to_microseconds;
use sntpc::NtpContext;
use sntpc::NtpTimestampGenerator;

use crate::wifi::NtpStack;
use crate::system_time::SystemTime;

const PACKET_METADATA_N: usize = 10;
const UDP_BUFFER_SIZE: usize = 1536;

#[derive(Clone, Copy)]
struct TimestampGen<'a> {
    system_time: &'a SystemTime,
    seconds: u64,
    sub_seconds: u32,
}

const US_PER_S: u64 = 1_000_000;

impl<'a> NtpTimestampGenerator for TimestampGen<'a> {
    fn init(&mut self) {
        let us = self.system_time.get_time_us();

        self.seconds = us / US_PER_S;
        self.sub_seconds = (us - (self.seconds * US_PER_S)) as u32;
        debug!("us: {}, seconds: {}, sub_seconds: {}, datetime: {:?}",
            us,
            self.seconds,
            self.sub_seconds,
            {
                use time::{ OffsetDateTime, UtcOffset };
                let time_as_nanos = us as i128 * 1000;
                OffsetDateTime::from_unix_timestamp_nanos(time_as_nanos).unwrap()
                    .checked_to_offset(UtcOffset::from_whole_seconds(3600).unwrap())
                    .unwrap()
            }
        )
    }

    fn timestamp_sec(&self) -> u64 {
        self.seconds
    }

    fn timestamp_subsec_micros(&self) -> u32 {
        self.sub_seconds
    }
}

pub struct Client {
    /// Wifi stack
    stack: NtpStack,

    rx_meta: [PacketMetadata; PACKET_METADATA_N],
    tx_meta: [PacketMetadata; PACKET_METADATA_N],
    rx_buffer: [u8; UDP_BUFFER_SIZE],
    tx_buffer: [u8; UDP_BUFFER_SIZE],

    ntp_socket: SocketAddrV4,
}

impl Client {
    /// Create a new client
    pub fn new(stack: NtpStack, ntp_ip: Ipv4Addr) -> Self {
        let rx_meta =  [PacketMetadata::EMPTY; PACKET_METADATA_N];
        let rx_buffer = [0u8; UDP_BUFFER_SIZE];
        let tx_meta = [PacketMetadata::EMPTY; PACKET_METADATA_N];
        let tx_buffer = [0u8; UDP_BUFFER_SIZE]; 

        let ntp_socket = SocketAddrV4::new(ntp_ip, NTP_PORT);

        Self {
            stack,
            rx_meta,
            rx_buffer,
            tx_meta,
            tx_buffer,

            ntp_socket,
        }
    }

    pub async fn new_from_dns(stack: NtpStack, dns_name: &str) -> Result<Self, Error> {
        trace!("Create DNS socket");
        let dns_socket = DnsSocket::new(stack.stack());
        
        trace!("Resolving dns name");
        let addresses = dns_socket.query(dns_name, DnsQueryType::A).await?;
        if addresses.len() == 0 {
            Err(Error::Other("DNS returned 0 A addresses"))?;
        }
        trace!("DNS result: {:?}", addresses);

        let IpAddress::Ipv4(ip) = addresses[0] else {
            return Err(Error::Other("Ipv6 address not supported"));
        };

        Ok(Self::new(stack, ip))
    }

    pub async fn ntp_request<'b>(&mut self, system_time: &'b mut SystemTime, use_offset: bool) -> Result<(u64, i64), Error> {
        // TODO: add timeout

        let mut socket = UdpSocket::new(self.stack.stack(), &mut self.rx_meta, &mut self.rx_buffer, &mut self.tx_meta, &mut self.tx_buffer);
        // 0 picks a random port
        socket.bind(0)?;
        
        let timestamp_gen = TimestampGen { system_time, seconds: 0, sub_seconds: 0 };
        let context = NtpContext::new(timestamp_gen);

        let r = get_time(
            self.ntp_socket.clone().into(), 
            &socket, 
            context,
        ).await?;

        trace!("NtpResult: {:?}", r);

        let ntp_time = {
            let sec = r.sec() as u64;
            let micros = fraction_to_microseconds(r.sec_fraction()) as u64;
            sec * 1_000_000 + micros 
        };

        let new_time = if use_offset {
            debug!("Updating using offset ({}s)", r.offset as f32 / 1_000_000.);
            if r.offset > 10 * 1_000_000 {
                warn!("Large NTP offset: {:.2}s", r.offset as f32 / 1_000_000.);
            }

            let now = system_time.get_time_us();
            if r.offset < 0 {
                let o = (-r.offset) as u64;
                now.wrapping_sub(o)
            } else {
                let o = r.offset as u64;
                now.wrapping_add(o)
            }
        } else {
            debug!("Updating using absolute time");
            ntp_time
        };

        debug!("new_time: {}, datetime: {:?}",
            new_time,
            {
                use time::{ OffsetDateTime, UtcOffset };
                let time_as_nanos = new_time as i128 * 1000;
                OffsetDateTime::from_unix_timestamp_nanos(time_as_nanos).unwrap()
                    .checked_to_offset(UtcOffset::from_whole_seconds(3600).unwrap())
                    .unwrap()
            }
        );
        debug!("ntp_time: {}, datetime: {:?}",
            ntp_time,
            {
                use time::{ OffsetDateTime, UtcOffset };
                let time_as_nanos = ntp_time as i128 * 1000;
                OffsetDateTime::from_unix_timestamp_nanos(time_as_nanos).unwrap()
                    .checked_to_offset(UtcOffset::from_whole_seconds(3600).unwrap())
                    .unwrap()
            }
        );

        let now = system_time.get_time_us();
        system_time.set_time_us(new_time);
        
        let adjustment = if now >= new_time {
            (now - new_time) as i64
        } else {
            (new_time - now) as i64
        };

        Ok((ntp_time, adjustment))
    }
}

/// An error within an NTP request
#[derive(Debug)]
#[allow(unused)]
pub enum Error {
    /// Response was too large
    ResponseTooLarge,

    /// Error within DNS system
    Dns(DnsError),

    Bind(BindError),

    Send(SendError),
    Recv(RecvError),

    Ntp(sntpc::Error),

    Other(&'static str),
}

impl From<sntpc::Error> for Error {
    fn from(value: sntpc::Error) -> Self {
        Self::Ntp(value)
    }
}
impl From<DnsError> for Error {
    fn from(error: DnsError) -> Self {
        Self::Dns(error)
    }
}

impl From<BindError> for Error {
    fn from(error: BindError) -> Self {
        Self::Bind(error)
    }
}

impl From<SendError> for Error {
    fn from(error: SendError) -> Self {
        Self::Send(error)
    }
}

impl From<RecvError> for Error {
    fn from(error: RecvError) -> Self {
        Self::Recv(error)
    }
}
