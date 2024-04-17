use bincode::Options;
use crossbeam::channel::{unbounded, RecvTimeoutError};
use solana_sdk::signature::Signature;
use solana_sdk::txingest::TxIngestMsg;
use std::collections::{HashMap, HashSet};
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

// Logged every 100 connection establishment protocol failures or every 1 second for a given remote address, whichever
// comes first.
// timestamp failed REMOTE_ADDRESS count

// Logged every 100 connection establishment failures due to remote peer exceeding connection limits, or every 1
// second for a given remote address, whichever comes first.
// timestamp exceeded REMOTE_ADDRESS stake count

// Logged on every start
// timestamp started REMOTE_ADDRESS:REMOTE_PORT stake

// Logged if num_vote_tx = 0 and num_user_tx = 0 after 10 seconds since started
// timestamp inactive REMOTE_ADDRESS:REMOTE_PORT stake

// Logged every time there are changes but no more frequently than once per 10 seconds for a given connection
// timestamp active REMOTE_ADDRESS:REMOTE_PORT stake duration num_dup_tx num_vote_tx num_user_tx num_badfee_tx \
//   num_fee_tx min_fee max_fee total_fees

// Logged when the local peer has closed the connection (most likely to make room for new connections) and its been
// at least 30 seconds since the most recent user tx received on the connection
// timestamp dropped REMOTE_ADDRESS:REMOTE_PORT stake duration num_dup_tx num_vote_tx num_user_tx num_badfee_tx \
//   num_fee_tx min_fee max_fee total_fees

// Logged when the local peer has closed the connection (most likely to make room for new connections) and its been
// at least 30 seconds since the most recent user tx received on the connection
// timestamp closed REMOTE_ADDRESS:REMOTE_PORT stake duration num_dup_tx num_vote_tx num_user_tx num_badfee_tx \
//   num_fee_tx min_fee max_fee total_fees

// Logged after a connection is complete, if it submitted any tx that were dups of tx submitted by another peer
// before it, listing all IPs that it submitted dups of
// timestamp dups REMOTE_ADDRESS:REMOTE_PORT REMOTE_ADDRESS ...

#[derive(Default)]
struct State
{
    // Failure counts
    pub failures : HashMap<IpAddr, Failures>,

    // Exceed counts
    pub exceeds : HashMap<IpAddr, Exceeds>,

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
struct Exceeds
{
    // timestamp of most recent log
    pub log_timestamp : u64,

    // stake of peer
    pub stake : u64,

    // true if there have been changes since last log
    pub changed : bool,

    // Number of new failures
    pub count : u64
}

enum ConnectionState
{
    // Open and active
    Open,

    // Pruned by the tx receiver
    Pruned,

    // Closed by remote peer
    Closed
}

struct Connection
{
    // Lamports of stake assigned to this connection (typically stake level of source, but can be altered by stake
    // overrides)
    pub stake : u64,

    // State of the connection - open or ended by local or peer
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

    // Number of dup tx submitted -- these are tx already submitted by a different peer
    pub dup_tx_count : u64,

    // IP addresses of other peers that had already submitted tx that this submitted dups of
    pub dup_peers : HashSet<IpAddr>,

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

    // Do periodic work: log stuff and clean.  Would be better to do it all based on timers instead of periodic
    // polling but this code isn't that sophisticated yet
    pub fn periodic(
        &mut self,
        now : u64
    )
    {
        // Remove any peer which hasn't had a failure in at least 10 seconds, and emit one final log for those peers
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

        // Remove any peer which hasn't had an exceed in at least 10 seconds, and emit one final log for those peers
        self.exceeds.retain(|peer_addr, exceeds| {
            if now >= (exceeds.log_timestamp + 10000) {
                if exceeds.count > 0 {
                    println!("{now} exceeded {peer_addr} {} {}", exceeds.stake, exceeds.count);
                }
                false
            }
            else {
                true
            }
        });

        // Remove any connection 30 seconds after it is ended (locally or by peer) -- it is assumed that all tx that
        // were submitted over the connection have had their "results" (fee or badfee) gathered by this point and that
        // the connection is no longer interesting.
        // Also remove any connection which is closed but never received a tx
        // Also log inactive on connections which are still inactive 10 seconds after they are started
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
                    // all fees to be assessed.  But don't retain if it never got a user tx because there's nothing
                    // left to gather for it.
                    if (connection.user_tx_count > 0) && (now < (connection.user_tx_timestamp + 30000)) {
                        true
                    }
                    else {
                        Self::log_active_or_ended(peer_addr, connection, &"dropped");
                        Self::log_dups(peer_addr, connection);
                        false
                    }
                },
                ConnectionState::Closed => {
                    // Retain it if it's not been 30 seconds since the last user tx submitted.  This gives time for
                    // all fees to be assessed.  But don't retain if it never got a user tx because there's nothing
                    // left to gather for it.
                    if (connection.user_tx_count > 0) && (now < (connection.user_tx_timestamp + 30000)) {
                        true
                    }
                    else {
                        Self::log_active_or_ended(peer_addr, connection, &"closed");
                        Self::log_dups(peer_addr, connection);
                        false
                    }
                }
            }
        });

        // Remove all txs whose connections have been removed
        self.txs.retain(|_, tx_peer_addr| self.connections.contains_key(tx_peer_addr));
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
        let num_dup_tx = connection.dup_tx_count;
        let num_vote_tx = connection.vote_tx_count;
        let num_user_tx = connection.user_tx_count;
        let num_badfee_tx = connection.badfee_tx_count;
        let num_fee_tx = connection.fee_tx_count;
        let min_fee = if connection.min_fee == u64::MAX { 0 } else { connection.min_fee };
        let max_fee = connection.max_fee;
        let total_fees = connection.total_fees;

        println!(
            "{timestamp} {action} {peer_addr} {stake} {duration} {num_dup_tx} {num_vote_tx} {num_user_tx} \
             {num_badfee_tx} {num_fee_tx} {min_fee} {max_fee} {total_fees}"
        );

        connection.changed = false;
        connection.log_timestamp = timestamp;
    }

    fn log_dups(
        peer_addr : &SocketAddr,
        connection : &Connection
    )
    {
        if connection.dup_peers.is_empty() {
            return;
        }

        let timestamp = now_millis();

        print!("{timestamp} dups {peer_addr} ");

        let mut space = false;
        for peer in &connection.dup_peers {
            if space {
                print!(" ");
            }
            else {
                space = true;
            }
            print!("{}", peer);
        }

        println!("");
    }

    pub fn failed(
        &mut self,
        _timestamp : u64,
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
        else if (failures.count == 100) || (now >= (failures.log_timestamp + 1000)) {
            println!("{now} failed {} {}", peer_addr.ip(), failures.count);
            failures.log_timestamp = now;
            failures.changed = false;
            failures.count = 0;
        }
        else {
            failures.changed = true;
        }
    }

    pub fn exceeded(
        &mut self,
        _timestamp : u64,
        peer_addr : SocketAddr,
        stake : u64
    )
    {
        let now = now_millis();

        let exceeds = self.exceeds.entry(peer_addr.ip().clone()).or_default();

        exceeds.stake = stake;

        exceeds.count += 1;

        if exceeds.log_timestamp == 0 {
            exceeds.log_timestamp = now;
            exceeds.changed = true;
        }
        else if (exceeds.count == 100) || (now >= (exceeds.log_timestamp + 1000)) {
            println!("{now} exceeded {} {stake} {}", peer_addr.ip(), exceeds.count);
            exceeds.log_timestamp = now;
            exceeds.changed = false;
            exceeds.count = 0;
        }
        else {
            exceeds.changed = true;
        }
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

            dup_tx_count : 0,

            dup_peers : Default::default(),

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

    pub fn closed(
        &mut self,
        _timestamp : u64,
        peer_addr : SocketAddr
    )
    {
        // It's possible that stake() was not called because of lost events
        if let Some(connection) = self.connections.get_mut(&peer_addr) {
            // If it was already pruned, then leave it in pruned state
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
        // It's possible that stake() was not called because of lost events
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
        let peer_addr = SocketAddr::new(peer_addr, peer_port);
        // It's possible that stake() was not called because of lost events
        if let Some(connection) = self.connections.get_mut(&peer_addr) {
            connection.user_tx_timestamp = now_millis();
            connection.user_tx_count += 1;
            if self.txs.contains_key(&signature) {
                // This is a dup submitted by a different peer
                connection.dup_tx_count += 1;
            }
            else {
                self.txs.insert(signature, peer_addr);
            }
            connection.changed = true;
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
            Ok(TxIngestMsg::Failed { timestamp, peer_addr }) => state.failed(timestamp, peer_addr),
            Ok(TxIngestMsg::Exceeded { timestamp, peer_addr, stake }) => state.exceeded(timestamp, peer_addr, stake),
            Ok(TxIngestMsg::Stake { timestamp, peer_addr, stake }) => state.stake(timestamp, peer_addr, stake),
            Ok(TxIngestMsg::Pruned { timestamp, peer_addr }) => state.pruned(timestamp, peer_addr),
            Ok(TxIngestMsg::Closed { timestamp, peer_addr }) => state.closed(timestamp, peer_addr),
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
