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

// Logged every time there are changes but no more frequently than once per 10 seconds for a given connection.
// The letter 'p' is logged in the 14th position if the connection is partial (i.e. was started before the receiver
// was started), and 'c' is logged there if the connection is complete (i.e. started after the receiver was started)
// timestamp active REMOTE_ADDRESS:REMOTE_PORT stake duration num_dup_tx num_vote_tx num_user_tx num_badfee_tx \
//   num_fee_tx min_fee max_fee total_fees <p_or_c>

// Logged when the local peer has closed the connection (most likely to make room for new connections) and its been
// at least 30 seconds since the most recent user tx received on the connection
// The letter 'p' is logged in the 14th position if the connection is partial (i.e. was started before the receiver
// was started), and 'c' is logged there if the connection is complete (i.e. started after the receiver was started)
// timestamp dropped REMOTE_ADDRESS:REMOTE_PORT stake duration num_dup_tx num_vote_tx num_user_tx num_badfee_tx \
//   num_fee_tx min_fee max_fee total_fees <p_or_c>
// 1         2       3                          4     5        6          7           8           9             \
//   10         11      12      13         14

// Logged when the local peer has closed the connection (most likely to make room for new connections) and its been
// at least 30 seconds since the most recent user tx received on the connection
// The letter 'p' is logged in the 14th position if the connection is partial (i.e. was started before the receiver
// was started), and 'c' is logged there if the connection is complete (i.e. started after the receiver was started)
// timestamp closed REMOTE_ADDRESS:REMOTE_PORT stake duration num_dup_tx num_vote_tx num_user_tx num_badfee_tx \
//   num_fee_tx min_fee max_fee total_fees <p_or_c>

// Logged whenever a transaction's paid fee becomes known.  This is only logged for tx that landed and that had a
// valid fee.  The original submitter of the tx is logged.
// timestamp fee REMOTE_ADDRESS:REMOTE_PORT signature lamports

// Logged after a connection is complete, if it submitted any tx that were dups of tx submitted by another peer
// before it, listing all IPs that it submitted dups of
// timestamp dups REMOTE_ADDRESS:REMOTE_PORT REMOTE_ADDRESS ...

#[derive(Default)]
struct State
{
    // Leader status -- how many slots until leader
    pub leader_status : LeaderStatus,

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
enum LeaderStatus
{
    // Not within 200 slots
    #[default]
    NotSoon,

    // Within 200 slots
    KindaSoon,

    // Within 20 slots
    Soon,

    // Within 2 slots
    VerySoon,

    // Now leader
    Leader
}

impl std::fmt::Display for LeaderStatus
{
    fn fmt(
        &self,
        f : &mut std::fmt::Formatter
    ) -> std::fmt::Result
    {
        match self {
            LeaderStatus::NotSoon => write!(f, "not"),
            LeaderStatus::KindaSoon => write!(f, "kinda"),
            LeaderStatus::Soon => write!(f, "soon"),
            LeaderStatus::VerySoon => write!(f, "very"),
            LeaderStatus::Leader => write!(f, "leader")
        }
    }
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

#[derive(Default)]
enum ConnectionState
{
    // Open and active
    #[default]
    Open,

    // Pruned by the tx receiver
    Pruned,

    // Closed by remote peer
    Closed
}

#[derive(Default)]
struct Connection
{
    // Was this connection created without having seen a "started" event?  This implies that the connection was made
    // before the receiver was connected to, so some events related to the connection were likely lost
    pub is_partial : bool,

    // Lamports of stake assigned to this connection (typically stake level of source, but can be altered by stake
    // overrides)
    pub stake : u64,

    // State of the connection - open or ended by local or peer
    pub state : ConnectionState,

    // timestamp of when connection was made
    pub start_timestamp : u64,

    // timestamp of when connection was ended
    pub end_timestamp : u64,

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

impl Connection
{
    pub fn new(
        stake : Option<u64>,
        start_timestamp : u64,
        log_timestamp : u64
    ) -> Self
    {
        Self {
            is_partial : stake.is_none(),

            stake : stake.unwrap_or(0),

            start_timestamp,

            log_timestamp,

            min_fee : u64::MAX,

            ..Connection::default()
        }
    }
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
        let duration = connection.end_timestamp.saturating_sub(connection.start_timestamp);
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
             {num_badfee_tx} {num_fee_tx} {min_fee} {max_fee} {total_fees} {}",
            if connection.is_partial { "p" } else { "c" }
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

        print!("{timestamp} dups {} ", peer_addr.ip());

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
        timestamp : u64,
        peer_addr : SocketAddr,
        stake : u64
    )
    {
        println!("{timestamp} started {peer_addr} {stake}");

        // Currently the validator can send multiple stake events per QUIC connection.  It is not clear how or why
        // this happens.  These events should only be sent when a new QUIC connection is established; but they are
        // sent for the same SocketAddr multiple times in a row.  For the time being, just ignore all the extra ones.
        if let Some(connection) = self.connections.get_mut(&peer_addr) {
            // If the connection was partial, then update its stake
            if connection.is_partial {
                connection.stake = stake;
            }
        }
        else {
            self.connections.insert(peer_addr, Connection::new(Some(stake), timestamp, now_millis()));
        }
    }

    pub fn pruned(
        &mut self,
        timestamp : u64,
        peer_addr : SocketAddr
    )
    {
        // It's possible that stake() was not called because of lost events
        if let Some(connection) = self.connections.get_mut(&peer_addr) {
            connection.end_timestamp = timestamp;
            connection.state = ConnectionState::Pruned;
        }
    }

    pub fn closed(
        &mut self,
        timestamp : u64,
        peer_addr : SocketAddr
    )
    {
        // It's possible that stake() was not called because of lost events
        if let Some(connection) = self.connections.get_mut(&peer_addr) {
            // If it was already pruned, then leave it in pruned state
            match connection.state {
                ConnectionState::Open => {
                    connection.end_timestamp = timestamp;
                    connection.state = ConnectionState::Closed;
                },
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
        let connection = self.connections.entry(SocketAddr::new(peer_addr, peer_port)).or_insert_with(|| {
            let now = now_millis();
            Connection::new(None, now, now)
        });
        connection.vote_tx_count += 1;
        connection.changed = true;
    }

    pub fn usertx(
        &mut self,
        show_tx : bool,
        timestamp : u64,
        peer_addr : IpAddr,
        peer_port : u16,
        signature : Signature
    )
    {
        let peer_addr = SocketAddr::new(peer_addr, peer_port);

        if show_tx {
            println!("{timestamp} tx {peer_addr}:{peer_port} {signature} {}", self.leader_status);
        }

        // It's possible that stake() was not called because of lost events; in that case, stake will not be known
        let connection = self.connections.entry(peer_addr.clone()).or_insert_with(|| {
            let now = now_millis();
            Connection::new(None, now, now)
        });
        connection.user_tx_timestamp = timestamp;
        connection.user_tx_count += 1;
        if let Some(already_peer) = self.txs.get(&signature) {
            connection.dup_tx_count += 1;
            if *already_peer != peer_addr {
                // This is a dup submitted by a different peer
                connection.dup_peers.insert(already_peer.ip().clone());
            }
        }
        else {
            self.txs.insert(signature, peer_addr);
        }
        connection.changed = true;
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
            let connection = self.connections.entry(sender.clone()).or_insert_with(|| {
                let now = now_millis();
                Connection::new(None, now, now)
            });
            connection.badfee_tx_count += 1;
            connection.changed = true;
        }
    }

    pub fn fee(
        &mut self,
        show_tx : bool,
        timestamp : u64,
        signature : Signature,
        fee : u64
    )
    {
        if let Some(sender) = self.txs.get(&signature) {
            if show_tx {
                println!("{timestamp} fee {sender} {signature} {fee}");
            }
            let connection = self.connections.entry(sender.clone()).or_insert_with(|| {
                let now = now_millis();
                Connection::new(None, now, now)
            });
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

    pub fn will_be_leader(
        &mut self,
        timestamp : u64,
        slots : u8
    )
    {
        println!("{timestamp} leader_upcoming {slots}");

        if slots <= 2 {
            self.leader_status = LeaderStatus::VerySoon;
        }
        else if slots <= 20 {
            self.leader_status = LeaderStatus::Soon;
        }
        else if slots <= 200 {
            self.leader_status = LeaderStatus::KindaSoon;
        }
        else {
            self.leader_status = LeaderStatus::NotSoon;
        }
    }

    pub fn begin_leader(
        &mut self,
        timestamp : u64
    )
    {
        println!("{timestamp} leader_begin");

        self.leader_status = LeaderStatus::Leader;
    }

    pub fn end_leader(
        &mut self,
        timestamp : u64
    )
    {
        println!("{timestamp} leader_end");

        self.leader_status = LeaderStatus::NotSoon;
    }
}

fn main()
{
    // Listen on a specific port; for the time being just dump events out to stdout

    let input_args = std::env::args().skip(1).collect::<Vec<String>>();

    if (input_args.len() < 2) || (input_args.len() > 3) {
        eprintln!("ERROR: Incorrect number of arguments: must be: [--show_tx] <LISTEN_ADDRESS> <LISTEN_PORT>");
        std::process::exit(-1);
    }

    let mut idx = 0;

    let mut show_tx = false;

    if input_args[0] == "--show_tx" {
        show_tx = true;
        idx = 1;
    }

    let addr = input_args[idx]
        .parse::<Ipv4Addr>()
        .unwrap_or_else(|e| error_exit(format!("ERROR: Invalid listen address {}: {e}", input_args[idx])));

    let port = input_args[idx + 1]
        .parse::<u16>()
        .unwrap_or_else(|e| error_exit(format!("ERROR: Invalid listen port {}: {e}", input_args[idx + 1])));

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
                state.usertx(show_tx, timestamp, peer_addr, peer_port, signature)
            },
            Ok(TxIngestMsg::Forwarded { timestamp, signature }) => state.forwarded(timestamp, signature),
            Ok(TxIngestMsg::BadFee { timestamp, signature }) => state.badfee(timestamp, signature),
            Ok(TxIngestMsg::Fee { timestamp, signature, fee }) => state.fee(show_tx, timestamp, signature, fee),
            Ok(TxIngestMsg::WillBeLeader { timestamp, slots }) => state.will_be_leader(timestamp, slots),
            Ok(TxIngestMsg::BeginLeader { timestamp }) => state.begin_leader(timestamp),
            Ok(TxIngestMsg::EndLeader { timestamp }) => state.end_leader(timestamp)
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
