use bincode::Options;
use crossbeam::channel::{unbounded, RecvTimeoutError};
use solana_sdk::signature::Signature;
use solana_sdk::txingest::TxIngestMsg;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::sync::Arc;

// Emit lines that represent the results of QUIC connections

// Logged every 100 failures or every 10 seconds for a given remote address, whichever comes first
// timestamp failed REMOTE_ADDRESS count

// Logged on every start
// timestamp started REMOTE_ADDRESS:REMOTE_PORT stake

// Logged if num_vote_tx = 0, num_user_tx = 0, and num_fwd_tx = 0 after 10 seconds since started
// timestamp inactive REMOTE_ADDRESS:REMOTE_PORT stake

// Logged every time there are changes but no more frequently than once per 10 seconds for a given connection
// num_user_tx is number of executed user tx; num_fwd_tx is number of tx not executed but forwarded (which can include
//   votes)
// timestamp active REMOTE_ADDRESS:REMOTE_PORT stake duration num_vote_tx num_user_tx num_fwd_tx min_fee max_fee avg_fee

// Logged on every end
// num_user_tx is number of executed user tx; num_fwd_tx is number of tx not executed but forwarded (which can include
//   votes)
// timestamp ended REMOTE_ADDRESS:REMOTE_PORT stake duration num_vote_tx num_user_tx num_fwd_tx min_fee max_fee avg_fee

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

#[derive(Default)]
struct Connection
{
    pub stake : u64,

    // timestamp of when connection was made
    pub start_timestamp : u64,

    // timestamp of most recent log
    pub log_timestamp : u64,

    // true if there have been changes since last log
    pub changed : bool,

    // true if "inactive" was logged
    pub inactive_logged : bool,

    // Number of vote tx received
    pub vote_tx_count : u64,

    // User transaction signature mapping to None if it was submitted but not executed, or mapping to Some(fee > 0) if
    // it executed and paid a fee, or mapping to Some(0) if forwarded
    pub txs : HashMap<Signature, Option<u64>>
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
                println!("{now} failed {peer_addr} {}", failures.count);
                false
            }
            else {
                true
            }
        });

        for (peer_addr, connection) in &mut self.connections {
            if connection.changed {
                if now >= (connection.log_timestamp + 10000) {
                    Self::log_active_or_ended(peer_addr, connection, &"active");
                }
            }
            else if !connection.inactive_logged {
                if now >= (connection.start_timestamp + 10000) {
                    println!("{now} inactive {peer_addr} {}", connection.stake);
                    connection.inactive_logged = true;
                }
            }
        }

        // Remove all txs associated whose connections have been removed
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
        let now = now_millis();

        // Counts user tx which actually were executed
        let mut num_user_tx = 0;
        // Counts user tx which were forwarded
        let mut num_fwd_tx = 0;
        let mut total_fees = 0;
        let mut min_fee = u64::MAX;
        let mut max_fee = 0;

        for (_, fee) in &connection.txs {
            match fee {
                None => (),
                Some(0) => {
                    num_fwd_tx += 1;
                },
                Some(fee) => {
                    num_user_tx += 1;
                    total_fees += fee;
                    if *fee < min_fee {
                        min_fee = *fee;
                    }
                    if *fee > max_fee {
                        max_fee = *fee;
                    }
                }
            }
        }

        let stake = connection.stake;
        let duration = now.saturating_sub(connection.start_timestamp);
        let num_votes = connection.vote_tx_count;
        let min_fee = if min_fee == u64::MAX { 0 } else { min_fee };
        let avg_fee = if num_user_tx == 0 { 0 } else { total_fees / num_user_tx };

        println!(
            "{now} {action} {peer_addr} {stake} {duration} {num_votes} {num_user_tx} {num_fwd_tx} {min_fee} {max_fee} \
             {avg_fee}"
        );

        connection.changed = false;
        connection.log_timestamp = now;
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

            start_timestamp : now,

            log_timestamp : now,

            changed : false,

            inactive_logged : false,

            vote_tx_count : 0,

            txs : Default::default()
        });
    }

    pub fn pruned(
        &mut self,
        _timestamp : u64,
        peer_addr : SocketAddr
    )
    {
        // It's possible that stake() was not called because of lost events
        if let Some(mut connection) = self.connections.remove(&peer_addr) {
            Self::log_active_or_ended(&peer_addr, &mut connection, &"ended");
        }
    }

    // Closed by remote peer, or closed by local peer after at least 2 seconds
    pub fn dropped(
        &mut self,
        _timestamp : u64,
        peer_addr : SocketAddr
    )
    {
        // It's possible that stake() was not called because of lost events
        if let Some(mut connection) = self.connections.remove(&peer_addr) {
            Self::log_active_or_ended(&peer_addr, &mut connection, &"ended");
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
        if let Some(connection) = self.connections.get_mut(&SocketAddr::new(peer_addr, peer_port)) {
            connection.txs.insert(signature, None);
            connection.changed = true;
        }
    }

    pub fn forwarded(
        &mut self,
        _timestamp : u64,
        signature : Signature
    )
    {
        if let Some(sender) = self.txs.get(&signature) {
            if let Some(connection) = self.connections.get_mut(sender) {
                connection.txs.insert(signature, Some(0));
                connection.changed = true;
            }
        }
    }

    pub fn badfee(
        &self,
        _timestamp : u64,
        _signature : Signature
    )
    {
        // Nothing to do
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
                connection.txs.insert(signature, Some(fee));
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
        match receiver.recv_timeout(std::time::Duration::from_millis(1000)) {
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
