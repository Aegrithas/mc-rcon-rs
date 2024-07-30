#![cfg_attr(docs_rs, feature(doc_auto_cfg))]
#![warn(missing_docs)]

//! This crate provides a client for Minecraft's RCON protocol as specified at <https://wiki.vg/RCON>.
//! 
//! Connect to a server with [`RconClient::connect`], log in with [`RconClient::log_in`], and then send your commands [`RconClient::send_command`].
//! For example:
//! 
//! ```no_run
//! # use std::error::Error;
//! # 
//! # use mc_rcon::RconClient;
//! # 
//! # fn main() -> Result<(), Box<dyn Error>> {
//! let client = RconClient::connect("localhost:25575")?;
//! client.log_in("SuperSecurePassword")?;
//! println!("{}", client.send_command("seed")?);
//! #   Ok(())
//! # }
//! ```
//! 
//! This example connects to a server running on localhost,
//! with RCON configured on port 25575 (or omitted, as that is the default port)
//! and with password `SuperSecurePassword`,
//! after which it uses Minecraft's `seed` command to query the world's generation seed.
//! 
//! Assuming that the server is configured accordingly, this program will print a response from the server like `Seed: [-1137927873379713691]`.
//! 
//! Note that, although RCON servers [can send multiple response packets](https://wiki.vg/RCON#Fragmentation), this crate currently does not handle that possibility.
//! If you need that functionality, please open an issue.

use std::{error::Error, fmt::{self, Debug, Display, Formatter}, io::{self, Read, Write}, mem::size_of, net::{TcpStream, ToSocketAddrs}, sync::atomic::{AtomicBool, AtomicI32, Ordering::SeqCst}};

use arrayvec::ArrayVec;

/// The default port used by Minecraft for RCON.
/// 
/// This crate does not use this value, it is simply here for convenience and completeness.
pub const DEFAULT_RCON_PORT: u16 = 25575;

/// The maximum number of payload bytes that an RCON server will accept.
/// 
/// If users of this crate try to send passwords or commands longer than this,
/// they will get a [`LogInError::PasswordTooLong`] or a [`CommandError::CommandTooLong`],
/// and nothing will be sent to the server.
pub const MAX_OUTGOING_PAYLOAD_LEN: usize = 1446; // does not include nul terminator

/// The maximum number of payload bytes that an RCON server will send in one packet.
/// 
/// Currently, users of this crate can expect command responses to have lengths less that or equal to this value,
/// though that may change in the future given that servers may send multiple response packets.
pub const MAX_INCOMING_PAYLOAD_LEN: usize = 4096; // does not include nul terminator

const HEADER_LEN: usize = 10;

const LOGIN_TYPE: i32 = 3;

const COMMAND_TYPE: i32 = 2;

/// A client that has connected to an RCON server.
/// 
/// See the [crate-level documentation](crate) for an example.
#[derive(Debug)]
pub struct RconClient {
  
  stream: TcpStream,
  next_id: AtomicI32,
  logged_in: AtomicBool
  
}

impl RconClient {
  
  /// Construct a `RconClient` and connect to a server at the given address.
  /// 
  /// See the [crate-level documentation](crate) for an example.
  /// 
  /// # Errors
  /// 
  /// This function errors if any I/O errors occur while setting up the connection.
  /// Most notably, if the server is not running or RCON is not enabled,
  /// this method will error with [`ConnectionRefused`](std::io::ErrorKind::ConnectionRefused).
  pub fn connect<A: ToSocketAddrs>(server_addr: A) -> io::Result<RconClient> {
    let stream = TcpStream::connect(server_addr)?;
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(None)?;
    Ok(RconClient { stream, next_id: AtomicI32::new(0), logged_in: AtomicBool::new(false) })
  }
  
  /// Returns whether this client is logged in.
  /// 
  /// Example:
  /// ```no_run
  /// # use std::error::Error;
  /// # use mc_rcon::RconClient;
  /// # 
  /// # fn main() -> Result<(), Box<dyn Error>> {
  /// let client = RconClient::connect("localhost:25575")?;
  /// assert!(!client.is_logged_in());
  /// client.log_in("SuperSecurePassword")?;
  /// assert!(client.is_logged_in());
  /// #   Ok(())
  /// # }
  /// ```
  pub fn is_logged_in(&self) -> bool {
    self.logged_in.load(SeqCst)
  }
  
  fn send_log_in(&self, password: &str) -> Result<(), LogInError> {
    if self.is_logged_in() {
      Err(LogInError::AlreadyLoggedIn)?
    }
    let SendResponse { good_auth, payload: _ } = self.send(LogInPacket, password)?;
    if good_auth {
      Ok(())
    } else {
      Err(LogInError::BadPassword)
    }
  }
  
  fn get_next_id(&self) -> i32 {
    let mut id = self.next_id.fetch_add(1, SeqCst);
    if id == -1 { // skip id -1 so that authentication failures can always be identified
      id = self.next_id.fetch_add(1, SeqCst)
    }
    id
  }
  
  fn send<K: PacketKind>(&self, kind: K, payload: &str) -> Result<SendResponse, SendError> {
    let _ = kind;
    if payload.len() > MAX_OUTGOING_PAYLOAD_LEN {
      Err(SendError::PayloadTooLong)?
    }
    
    const I32_LEN: usize = size_of::<i32>();
    
    let out_len = i32::try_from(HEADER_LEN + payload.len()).expect("payload is too long");
    let out_id = self.get_next_id();
    
    let mut stream = &self.stream;
    // Buffering this apparently helps prevent MC from reading a packet of length < 10 and consequently disconnecting
    // I could use BufWriter, but in this case I know the exact max size, so this is probably cheaper (and I just like ArrayVec, and consequently take every opportunity to use it)
    let mut out_buf: ArrayVec<u8, {I32_LEN + HEADER_LEN + MAX_OUTGOING_PAYLOAD_LEN}> = ArrayVec::new();
    out_buf.write_all(&out_len.to_le_bytes())?;
    out_buf.write_all(&out_id.to_le_bytes())?;
    out_buf.write_all(&K::TYPE.to_le_bytes())?;
    out_buf.write_all(payload.as_bytes())?;
    out_buf.write_all(b"\0\0")?; // null terminator and padding
    debug_assert_eq!(out_buf.len(), I32_LEN + HEADER_LEN + payload.len());
    stream.write_all(&mut out_buf)?;
    stream.flush()?;
    
    let mut in_len_bytes = [0; I32_LEN];
    let mut in_id_bytes = [0; I32_LEN];
    stream.read_exact(&mut in_len_bytes)?;
    let in_len = i32::from_le_bytes(in_len_bytes);
    stream.read_exact(&mut in_id_bytes)?;
    let in_id = i32::from_le_bytes(in_id_bytes);
    stream.read_exact(&mut [0; I32_LEN])?;
    let payload_len = usize::try_from(in_len).expect("payload is too long") - HEADER_LEN;
    let mut payload_buf = vec![0; payload_len];
    stream.read_exact(&mut payload_buf)?;
    stream.read_exact(&mut [0; 2])?; // expect null terminator and padding
      
    let good_auth = if in_id == -1 {
      false
    } else if in_id == out_id {
      true
    } else {
      Err(io::Error::new(io::ErrorKind::InvalidData, K::INVLID_RESPONSE_ID_ERROR))?
    };
    
    if K::ACCEPTS_LONG_RESPONSES && payload_len >= MAX_INCOMING_PAYLOAD_LEN {
      const CAP_COMMAND: &'static str = "seed";
      let cap_len = i32::try_from(HEADER_LEN + CAP_COMMAND.len()).expect("cap payload is somehow too long");
      let cap_id = self.get_next_id();
      let mut cap_buf: ArrayVec<u8, {I32_LEN + HEADER_LEN + CAP_COMMAND.len()}> = ArrayVec::new();
      cap_buf.write_all(&cap_len.to_le_bytes())?;
      cap_buf.write_all(&cap_id.to_le_bytes())?;
      cap_buf.write_all(&K::TYPE.to_le_bytes())?;
      cap_buf.write_all(CAP_COMMAND.as_bytes())?;
      cap_buf.write_all(b"\0\0")?;
      debug_assert_eq!(cap_buf.len(), I32_LEN + HEADER_LEN + CAP_COMMAND.len());
      stream.write_all(&mut cap_buf)?;
      stream.flush()?;
      
      loop {
        stream.read_exact(&mut in_len_bytes)?;
        let inner_in_len = i32::from_le_bytes(in_len_bytes);
        stream.read_exact(&mut in_id_bytes)?;
        let inner_in_id = i32::from_le_bytes(in_id_bytes);
        stream.read_exact(&mut [0; I32_LEN])?;
        let inner_payload_len = usize::try_from(inner_in_len).expect("payload is too long") - HEADER_LEN;
        let mut inner_payload_buf = vec![0; inner_payload_len];
        stream.read_exact(&mut inner_payload_buf)?;
        stream.read_exact(&mut [0; 2])?;
        
        if inner_in_id == cap_id {
          break
        } else if inner_in_id == in_id {
          payload_buf.append(&mut inner_payload_buf);
        } else if inner_in_id == -1 {
          Err(io::Error::new(io::ErrorKind::InvalidData, "client became deauthenticated between packets"))?
        } else {
          Err(io::Error::new(io::ErrorKind::InvalidData, K::INVLID_RESPONSE_ID_ERROR))?
        }
      }
    }
    
    let payload = String::from_utf8(payload_buf).expect("response payload is not ASCII");
    Ok(SendResponse { good_auth, payload })
  }
  
  /// Attempts to log into the server with the given password.
  /// 
  /// See the [crate-level documentation](crate) for an example.
  /// 
  /// # Errors
  /// 
  /// * If the password is longer than [`MAX_OUTGOING_PAYLOAD_LEN`], returns [`LogInError::PasswordTooLong`] and does not send anything to the server.
  /// * If this client is already logged in, returns [`LogInError::AlreadyLoggedIn`] and does not send anything to the server.
  /// * If the given password is successfully sent, and the server responds indicating failure, returns [`LogInError::BadPassword`].
  /// * If any I/O errors occur, returns [`LogInError::IO`] with the error.
  ///   This notably includes [`ConnectionAborted`](std::io::ErrorKind::ConnectionAborted) if the server has closed the connection.
  pub fn log_in(&self, password: &str) -> Result<(), LogInError> {
    self.send_log_in(password)?;
    self.logged_in.store(true, SeqCst);
    Ok(())
  }
  
  /// Sends the given command to the server and returns its response.
  /// 
  /// See the [crate-level documentation](crate) for an example.
  /// 
  /// Valid commands are dependent on the server;
  /// documentation on the commands offered by vanilla Minecraft can be found at <https://minecraft.wiki/w/Commands>.
  /// Servers with mods or plugins may have other commands available.
  /// 
  /// The return value of this method is the response message from the server.
  /// This crate makes no attempt to interpret that response;
  /// in particular, this method will never error to indicate that the command failed:
  /// a successful return only means that the server recieved this command and responded to it.
  /// 
  /// # Errors
  /// 
  /// * If the command is longer than [`MAX_OUTGOING_PAYLOAD_LEN`], returns [`CommandError::CommandTooLong`] and does not send anything to the server.
  /// * If this client is not logged in, returns [`CommandError::NotLoggedIn`] and does not send anything to the server.
  /// * If any I/O errors occur, returns [`CommandError::IO`] with the error.
  ///   This notably includes [`ConnectionAborted`](std::io::ErrorKind::ConnectionAborted) if the server has closed the connection.
  pub fn send_command(&self, command: &str) -> Result<String, CommandError> {
    if !self.is_logged_in() {
      Err(CommandError::NotLoggedIn)?
    }
    let SendResponse { good_auth, payload } = self.send(CommandPacket, command)?;
    if good_auth {
      Ok(payload)
    } else {
      Err(CommandError::NotLoggedIn)
    }
  }
  
}

trait PacketKind {
  
  const ACCEPTS_LONG_RESPONSES: bool;
  
  const TYPE: i32;
  
  const INVLID_RESPONSE_ID_ERROR: &'static str;
  
}

struct LogInPacket;

impl PacketKind for LogInPacket {
  
  const ACCEPTS_LONG_RESPONSES: bool = false;
  
  const TYPE: i32 = LOGIN_TYPE;
  
  const INVLID_RESPONSE_ID_ERROR: &'static str = "response packet id mismatched with login packet id";
  
}

struct CommandPacket;

impl PacketKind for CommandPacket {
  
  const ACCEPTS_LONG_RESPONSES: bool = true;
  
  const TYPE: i32 = COMMAND_TYPE;
  
  const INVLID_RESPONSE_ID_ERROR: &'static str = "response packet id mismatched with command packet id";
  
}

#[derive(Debug)]
struct SendResponse {
  
  good_auth: bool,
  payload: String
  
}

/// A failed attempt to log in. See [`RconClient::log_in`] for details.
#[derive(Debug)]
pub enum LogInError {
  
  /// An I/O error occured.
  IO(io::Error),
  /// The password was too long.
  PasswordTooLong,
  /// The client is already logged in.
  AlreadyLoggedIn,
  /// The password was incorrect.
  BadPassword
  
}

impl From<io::Error> for LogInError {
  
  fn from(e: io::Error) -> Self {
    LogInError::IO(e)
  }
  
}

impl From<SendError> for LogInError {
  
  fn from(e: SendError) -> Self {
    match e {
      SendError::IO(e) => LogInError::IO(e),
      SendError::PayloadTooLong => LogInError::PasswordTooLong
    }
  }
  
}

impl Display for LogInError {
  
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    match self {
      LogInError::IO(e) => Display::fmt(e, f),
      LogInError::PasswordTooLong => write!(f, "password must be no longer than {} bytes", MAX_OUTGOING_PAYLOAD_LEN),
      LogInError::AlreadyLoggedIn => write!(f, "tried to log in when already logged in"),
      LogInError::BadPassword => write!(f, "tried to log in with incorrect password")
    }
  }
  
}

impl Error for LogInError {}

/// A failed attempt to send a command. See [`RconClient::send_command`] for details.
#[derive(Debug)]
pub enum CommandError {
  
  /// An I/O error occurred.
  IO(io::Error),
  /// The command was too long.
  CommandTooLong,
  /// The client is not logged in.
  NotLoggedIn
  
}

impl From<io::Error> for CommandError {
  
  fn from(e: io::Error) -> Self {
    CommandError::IO(e)
  }
  
}

impl From<SendError> for CommandError {
  
  fn from(e: SendError) -> Self {
    match e {
      SendError::IO(e) => CommandError::IO(e),
      SendError::PayloadTooLong => CommandError::CommandTooLong
    }
  }
  
}

impl Display for CommandError {
  
  fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
    match self {
      CommandError::IO(e) => Display::fmt(e, f),
      CommandError::CommandTooLong => write!(f, "command must be no longer than {} bytes", MAX_OUTGOING_PAYLOAD_LEN),
      CommandError::NotLoggedIn => write!(f, "tried to send a command before logging in")
    }
  }
  
}

impl Error for CommandError {}

#[derive(Debug)]
enum SendError {
  
  IO(io::Error),
  PayloadTooLong
  
}

impl From<io::Error> for SendError {
  
  fn from(e: io::Error) -> Self {
    SendError::IO(e)
  }
  
}