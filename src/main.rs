use bincode::Options;
use solana_sdk::txingest::TxIngestMsg;
use std::net::{Ipv4Addr, TcpListener};

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

    loop {
        let mut tcp_stream = loop {
            match tcp_listener.accept() {
                Ok((tcp_stream, _)) => break tcp_stream,
                Err(e) => eprintln!("Failed accept because {e}")
            }
        };

        let options = bincode::DefaultOptions::new();

        loop {
            match options.deserialize_from::<_, TxIngestMsg>(&mut tcp_stream) {
                Ok(tx_ingest_msg) => {
                    print_tx_ingest_msg(&tx_ingest_msg);
                },
                Err(e) => {
                    eprintln!("Failed deserialize because {e}; closing connection");
                    tcp_stream.shutdown(std::net::Shutdown::Both).ok();
                    break;
                }
            }
        }
    }
}

fn error_exit(msg : String) -> !
{
    eprintln!("{msg}");
    std::process::exit(-1);
}

fn print_tx_ingest_msg(msg : &TxIngestMsg)
{
    match msg {
        TxIngestMsg::Exceeded { timestamp, peer_addr } => {
            println!("{timestamp} exceeded {peer_addr}");
        },
        TxIngestMsg::Disallowed { timestamp, peer_addr } => {
            println!("{timestamp} disallowed {peer_addr}");
        },
        TxIngestMsg::TooMany { timestamp, peer_addr } => {
            println!("{timestamp} toomany {peer_addr}");
        },
        TxIngestMsg::Stake { timestamp, peer_addr, stake } => {
            println!("{timestamp} stake {peer_addr} {stake}");
        },
        TxIngestMsg::Pruned { timestamp, peer_addr } => {
            println!("{timestamp} pruned {peer_addr}");
        },
        TxIngestMsg::Dropped { timestamp, peer_addr } => {
            println!("{timestamp} dropped {peer_addr}");
        },
        TxIngestMsg::VoteTx { timestamp, peer_addr, peer_port } => {
            println!("{timestamp} votetx {peer_addr}:{peer_port} ");
        },
        TxIngestMsg::UserTx { timestamp, peer_addr, peer_port, signature } => {
            println!("{timestamp} usertx {peer_addr}:{peer_port} {signature}");
        },
        TxIngestMsg::Forwarded { timestamp, signature } => {
            println!("{timestamp} forwarded {signature}");
        },
        TxIngestMsg::BadFee { timestamp, signature } => {
            println!("{timestamp} badfee {signature}");
        },
        TxIngestMsg::Fee { timestamp, signature, fee } => {
            println!("{timestamp} fee {signature} {fee}");
        }
    }
}
