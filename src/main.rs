use bincode::Options;
use crossbeam::channel::{unbounded, RecvTimeoutError};
use solana_sdk::signature::Signature;
use solana_sdk::txingest::TxIngestMsg;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::sync::Arc;

// Track whether or not any tx were submitted on a QUIC connection

// Then also track for every tx whether it was:
// - Forwarded
// - Bad Fee
// - Fee

// Need to track every tx through to either Forwarded, Bad Fee, or Fee.  Only when all have been figured out,
// or there is some timeout

// Emit lines that represent the results of QUIC connections

// Logged every 100 failures or every 10 seconds for a given remote address, whichever comes first
// timestamp failed REMOTE_ADDRESS count

// Logged on every start
// timestamp started REMOTE_ADDRESS:REMOTE_PORT stake

// Logged if num_vote_tx = 0 and num_user_tx = 0 after 10 seconds since started
// timestamp inactive REMOTE_ADDRESS:REMOTE_PORT stake

// Logged every time there are changes but no more frequently than once per 10 seconds for a given connection
// timestamp active REMOTE_ADDRESS:REMOTE_PORT stake duration num_vote_tx num_user_tx num_badfee_tx num_fee_tx min_fee max_fee total_fees

// Logged when the local peer has removed the connection to make room for others and there have been no user tx for at
// least 10 seconds
// timestamp pruned REMOTE_ADDRESS:REMOTE_PORT stake duration num_vote_tx num_user_tx num_badfee_tx num_fee_tx min_fee max_fee total_fees

// Logged when the remote peer has closed the connection and there have been no user tx for at least 10 seconds
// timestamp closed REMOTE_ADDRESS:REMOTE_PORT stake duration num_vote_tx num_user_tx num_badfee_tx num_fee_tx min_fee max_fee total_fees

#[derive(Default)]
struct State
{
    // Failure counts
    pub failures : HashMap<IpAddr, Failures>,

    // Current connections
    pub connections : HashMap<SocketAddr, Connection>,

    // Map from tx to the SocketAddr that submitted it
    pub txs : HashMap<Signature, SocketAddr>
}

#[derive(Default)]
struct Failures
{
    // timestamp of most recent log
    pub log_timestamp : u64,

    // true if there have been changes since last log
    pub changed : bool,

    // Number of new failures
    pub count : u64
}

enum ConnectionState
{
    Open,

    // Pruned by local peer
    Pruned,

    // Closed by remote peer
    Closed
}

struct Connection
{
    pub stake : u64,

    pub state : ConnectionState,

    // timestamp of when connection was made
    pub start_timestamp : u64,

    // timestamp of user tx most recently received
    pub user_tx_timestamp : u64,

    // true if "inactive" was logged
    pub inactive_logged : bool,

    // timestamp of most recent log
    pub log_timestamp : u64,

    // Changed since last log?
    pub changed : bool,

    // Number of vote tx received
    pub vote_tx_count : u64,

    // Number of user tx received from QUIC peer.  Note that it's possible for a user tx to be received but not
    // determine badfee or fee for it; it's *probably* the case that it got forwarded.
    pub user_tx_count : u64,

    // Number of "bad fee" tx received
    pub badfee_tx_count : u64,

    // Number of fee paying tx received
    pub fee_tx_count : u64,

    // Minimum fee
    pub min_fee : u64,

    // Maximum fee
    pub max_fee : u64,

    // Total fees
    pub total_fees : u64
}

fn now_millis() -> u64
{
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}

impl State
{
    pub fn new() -> Self
    {
        Self::default()
    }

    // Do periodic work: log stuff and clean
    pub fn periodic(
        &mut self,
        now : u64
    )
    {
        self.failures.retain(|peer_addr, failures| {
            if now >= (failures.log_timestamp + 10000) {
                if failures.count > 0 {
                    println!("{now} failed {peer_addr} {}", failures.count);
                }
                false
            }
            else {
                true
            }
        });

        self.connections.retain(|peer_addr, connection| {
            match connection.state {
                ConnectionState::Open => {
                    if connection.changed {
                        if now >= (connection.log_timestamp + 10000) {
                            Self::log_active_or_ended(peer_addr, connection, &"active");
                        }
                    }
                    else if !connection.inactive_logged && (now >= (connection.start_timestamp + 10000)) {
                        println!("{now} inactive {peer_addr} {}", connection.stake);
                        connection.inactive_logged = true;
                    }
                    true
                },
                ConnectionState::Pruned => {
                    // Retain it if it's not been 30 seconds since the last user tx submitted.  This gives time for
                    // all fees to be assessed.
                    if now < (connection.user_tx_timestamp + 30000) {
                        true
                    }
                    else {
                        Self::log_active_or_ended(peer_addr, connection, &"pruned");
                        false
                    }
                },
                ConnectionState::Closed => {
                    // Retain it if it's not been 30 seconds since the last user tx submitted.  This gives time for
                    // all fees to be assessed.
                    if now < (connection.user_tx_timestamp + 30000) {
                        true
                    }
                    else {
                        Self::log_active_or_ended(peer_addr, connection, &"closed");
                        false
                    }
                }
            }
        });

        // Remove all txs whose connections have been removed
        self.txs.retain(|_, tx_peer_addr| self.connections.contains_key(tx_peer_addr));
    }

    fn failed(
        &mut self,
        peer_addr : SocketAddr
    )
    {
        let now = now_millis();

        let failures = self.failures.entry(peer_addr.ip().clone()).or_default();

        failures.count += 1;

        if failures.log_timestamp == 0 {
            failures.log_timestamp = now;
            failures.changed = true;
        }
        else if (failures.count == 100) || (now >= (failures.log_timestamp + 10000)) {
            println!("{now} failed {} {}", peer_addr.ip(), failures.count);
            failures.log_timestamp = now;
            failures.changed = false;
            failures.count = 0;
        }
        else {
            failures.changed = true;
        }
    }

    fn log_active_or_ended(
        peer_addr : &SocketAddr,
        connection : &mut Connection,
        action : &str
    )
    {
        let timestamp = now_millis();

        let stake = connection.stake;
        let duration = timestamp.saturating_sub(connection.start_timestamp);
        let num_vote_tx = connection.vote_tx_count;
        let num_user_tx = connection.user_tx_count;
        let num_badfee_tx = connection.badfee_tx_count;
        let num_fee_tx = connection.fee_tx_count;
        let min_fee = if connection.min_fee == u64::MAX { 0 } else { connection.min_fee };
        let max_fee = connection.max_fee;
        let total_fees = connection.total_fees;

        println!(
            "{timestamp} {action} {peer_addr} {stake} {duration} {num_vote_tx} {num_user_tx} {num_badfee_tx} \
             {num_fee_tx} {min_fee} {max_fee} {total_fees}"
        );

        connection.changed = false;
        connection.log_timestamp = timestamp;
    }

    pub fn exceeded(
        &mut self,
        _timestamp : u64,
        peer_addr : SocketAddr
    )
    {
        self.failed(peer_addr);
    }

    pub fn disallowed(
        &mut self,
        _timestamp : u64,
        peer_addr : SocketAddr
    )
    {
        self.failed(peer_addr);
    }

    pub fn toomany(
        &mut self,
        _timestamp : u64,
        peer_addr : SocketAddr
    )
    {
        self.failed(peer_addr);
    }

    pub fn stake(
        &mut self,
        _timestamp : u64,
        peer_addr : SocketAddr,
        stake : u64
    )
    {
        let now = now_millis();

        println!("{now} started {peer_addr} {stake}");

        self.connections.insert(peer_addr, Connection {
            stake,

            state : ConnectionState::Open,

            start_timestamp : now,

            user_tx_timestamp : 0,

            log_timestamp : now,

            changed : false,

            inactive_logged : false,

            vote_tx_count : 0,

            user_tx_count : 0,

            badfee_tx_count : 0,

            fee_tx_count : 0,

            min_fee : u64::MAX,

            max_fee : 0,

            total_fees : 0
        });
    }

    pub fn pruned(
        &mut self,
        _timestamp : u64,
        peer_addr : SocketAddr
    )
    {
        // It's possible that stake() was not called because of lost events
        if let Some(connection) = self.connections.get_mut(&peer_addr) {
            connection.state = ConnectionState::Pruned;
        }
    }

    pub fn dropped(
        &mut self,
        _timestamp : u64,
        peer_addr : SocketAddr
    )
    {
        // It's possible that stake() was not called because of lost events
        if let Some(connection) = self.connections.get_mut(&peer_addr) {
            match connection.state {
                ConnectionState::Open => connection.state = ConnectionState::Closed,
                _ => ()
            }
        }
    }

    pub fn votetx(
        &mut self,
        _timestamp : u64,
        peer_addr : IpAddr,
        peer_port : u16
    )
    {
        // If the connection is unknown, ignore
        if let Some(connection) = self.connections.get_mut(&SocketAddr::new(peer_addr, peer_port)) {
            connection.vote_tx_count += 1;
            connection.changed = true;
        }
    }

    pub fn usertx(
        &mut self,
        _timestamp : u64,
        peer_addr : IpAddr,
        peer_port : u16,
        signature : Signature
    )
    {
        // If the connection is unknown, ignore
        let peer_addr = SocketAddr::new(peer_addr, peer_port);
        if let Some(connection) = self.connections.get_mut(&peer_addr) {
            connection.user_tx_timestamp = now_millis();
            connection.user_tx_count += 1;
            // If the signature is already known, ignore; all subsequent are dups
            self.txs.entry(signature).or_insert(peer_addr);
        }
    }

    pub fn forwarded(
        &mut self,
        _timestamp : u64,
        _signature : Signature
    )
    {
        // Don't care
    }

    pub fn badfee(
        &mut self,
        _timestamp : u64,
        signature : Signature
    )
    {
        if let Some(sender) = self.txs.get(&signature) {
            if let Some(connection) = self.connections.get_mut(sender) {
                connection.badfee_tx_count += 1;
                connection.changed = true;
            }
        }
    }

    pub fn fee(
        &mut self,
        _timestamp : u64,
        signature : Signature,
        fee : u64
    )
    {
        if let Some(sender) = self.txs.get(&signature) {
            if let Some(connection) = self.connections.get_mut(sender) {
                connection.fee_tx_count += 1;
                if fee < connection.min_fee {
                    connection.min_fee = fee;
                }
                if fee > connection.max_fee {
                    connection.max_fee = fee;
                }
                connection.total_fees += fee;
                connection.changed = true;
            }
        }
    }
}

fn main()
{
    // Listen on a specific port; for the time being just dump events out to stdout

    let input_args = std::env::args().skip(1).collect::<Vec<String>>();

    if input_args.len() != 2 {
        eprintln!("ERROR: Incorrect number of arguments: must be: <LISTEN_ADDRESS> <LISTEN_PORT>");
        std::process::exit(-1);
    }

    let addr = input_args[0]
        .parse::<Ipv4Addr>()
        .unwrap_or_else(|e| error_exit(format!("ERROR: Invalid listen address {}: {e}", input_args[0])));

    let port = input_args[1]
        .parse::<u16>()
        .unwrap_or_else(|e| error_exit(format!("ERROR: Invalid listen port {}: {e}", input_args[1])));

    // Listen
    let tcp_listener = loop {
        match TcpListener::bind(std::net::SocketAddr::V4(std::net::SocketAddrV4::new(addr, port))) {
            Ok(tcp_listener) => break tcp_listener,
            Err(e) => {
                eprintln!("Failed bind because {e}, trying again in 1 second");
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
    };

    let (sender, receiver) = unbounded::<TxIngestMsg>();

    let sender = Arc::new(sender);

    // Spawn the listener
    std::thread::spawn(move || {
        loop {
            let mut tcp_stream = loop {
                match tcp_listener.accept() {
                    Ok((tcp_stream, _)) => break tcp_stream,
                    Err(e) => eprintln!("Failed accept because {e}")
                }
            };

            {
                let sender = sender.clone();

                // Spawn a thread to handle this TCP stream.  Multiple streams are accepted at once, to allow e.g.
                // a JITO relayer and a validator to both connect.
                std::thread::spawn(move || {
                    let options = bincode::DefaultOptions::new();

                    loop {
                        match options.deserialize_from::<_, TxIngestMsg>(&mut tcp_stream) {
                            Ok(tx_ingest_msg) => sender.send(tx_ingest_msg).expect("crossbeam failed"),
                            Err(e) => {
                                eprintln!("Failed deserialize because {e}; closing connection");
                                tcp_stream.shutdown(std::net::Shutdown::Both).ok();
                                break;
                            }
                        }
                    }
                });
            }
        }
    });

    let mut state = State::new();

    let mut last_log_timestamp = 0;

    loop {
        // Receive with a timeout
        match receiver.recv_timeout(std::time::Duration::from_millis(100)) {
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => (),
            Ok(TxIngestMsg::Exceeded { timestamp, peer_addr }) => state.exceeded(timestamp, peer_addr),
            Ok(TxIngestMsg::Disallowed { timestamp, peer_addr }) => state.disallowed(timestamp, peer_addr),
            Ok(TxIngestMsg::TooMany { timestamp, peer_addr }) => state.toomany(timestamp, peer_addr),
            Ok(TxIngestMsg::Stake { timestamp, peer_addr, stake }) => state.stake(timestamp, peer_addr, stake),
            Ok(TxIngestMsg::Pruned { timestamp, peer_addr }) => state.pruned(timestamp, peer_addr),
            Ok(TxIngestMsg::Dropped { timestamp, peer_addr }) => state.dropped(timestamp, peer_addr),
            Ok(TxIngestMsg::VoteTx { timestamp, peer_addr, peer_port }) => {
                state.votetx(timestamp, peer_addr, peer_port)
            },
            Ok(TxIngestMsg::UserTx { timestamp, peer_addr, peer_port, signature }) => {
                state.usertx(timestamp, peer_addr, peer_port, signature)
            },
            Ok(TxIngestMsg::Forwarded { timestamp, signature }) => state.forwarded(timestamp, signature),
            Ok(TxIngestMsg::BadFee { timestamp, signature }) => state.badfee(timestamp, signature),
            Ok(TxIngestMsg::Fee { timestamp, signature, fee }) => state.fee(timestamp, signature, fee)
        }

        let now = now_millis();
        if now < (last_log_timestamp + 10000) {
            continue;
        }

        state.periodic(now);

        last_log_timestamp = now;
    }
}

fn error_exit(msg : String) -> !
{
    eprintln!("{msg}");
    std::process::exit(-1);
}
