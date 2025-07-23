use std::{
    io::{Error, ErrorKind, Result},
    net::{IpAddr, Ipv6Addr, SocketAddr},
    ops::Range,
    path::PathBuf,
};

use clap::{Parser, Subcommand};
use color_print::ceprintln;
use futures::executor::block_on;

use n3server::N3;

fn parse_port_range(arg: &str) -> std::result::Result<Range<u16>, String> {
    let parts = arg.split(":").collect::<Vec<_>>();

    match parts.len() {
        1 => {
            let port = parts[0]
                .parse::<u16>()
                .map_err(|err| format!("failed to parse port: {}", err.to_string()))?;

            return Ok(port..port + 1);
        }
        2 => {
            let from = parts[0]
                .parse::<u16>()
                .map_err(|err| format!("failed to parse port(from): {}", err.to_string()))?;

            let to = parts[1]
                .parse::<u16>()
                .map_err(|err| format!("failed to parse port(to): {}", err.to_string()))?;

            if !(to > from) {
                return Err(format!("failed to parse port range: ensure `to > from`"));
            }

            return Ok(from..to);
        }
        _ => {
            return Err("Invalid port range, valid syntax: `xxx:xxx` or `xxx`".to_owned());
        }
    }
}

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Support application protos.
    #[arg(long, value_name = "PROTO_LIST", default_values_t = ["n3".to_string()])]
    protos: Vec<String>,

    /// Specify the listening interface by ip
    #[arg(short, long, value_name = "ADDRESS")]
    interfaces: Option<Vec<IpAddr>>,

    /// Specify the listening port range: `from:to` or `port`
    #[arg(short, long, value_name = "PORT-RANGE", value_parser=parse_port_range)]
    ports: Range<u16>,

    /// Configure the certificate chain file(PEM).
    #[arg(short, long, value_name = "PEM_FILE", default_value = "n3.crt")]
    cert: PathBuf,

    /// Configure the private chain file(PEM).
    #[arg(short, long, value_name = "PEM_FILE", default_value = "n3.key")]
    key: PathBuf,

    /// Specifies a file where trusted CA certificates are stored for the purposes of peer's certificate verification.
    #[arg(short, long, value_name = "PEM_FILE")]
    verify_peer: Option<PathBuf>,

    /// Sets the quiche `initial_max_streams_bidi` transport parameter.
    ///
    /// When set to a non-zero value quiche will only allow v number of concurrent remotely-initiated bidirectional
    /// streams to be open at any given time and will increase the limit automatically as streams are completed.
    #[arg(long, value_name = "STREAMS", default_value_t = 100)]
    max_streams: u64,

    /// Sets the initial_max_stream_data_bidi_remote transport parameter.
    ///
    /// When set to a non-zero value quiche will only allow at most v bytes of incoming stream data
    /// to be buffered for each remotely-initiated bidirectional stream (that is, data that is not
    /// yet read by the application) and will allow more data to be received as the buffer is
    /// consumed by the application.
    ///
    /// When set to zero, either explicitly or via the default, quiche will not give any flow control
    /// to the peer, preventing it from sending any stream data.
    #[arg(long, value_name = "SIZE", default_value_t = 1024 * 1024 * 10)]
    max_stream_data_bidi: u64,

    /// Sets the max_idle_timeout transport parameter, in milliseconds.
    #[arg(long, value_name = "SIZE", default_value_t = 60 * 1000)]
    max_idle_timeout: u64,

    /// Debug mode, print verbose output informations.
    #[arg(short, long, default_value_t = false, action)]
    debug: bool,

    #[command(subcommand)]
    commands: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Configure the static redirection function
    Redirect {
        /// Specify the redirect target address
        target: SocketAddr,
    },
}

fn parse_laddrs(cli: &Cli) -> Result<Vec<SocketAddr>> {
    let interfaces = if let Some(interfaces) = cli.interfaces.clone() {
        interfaces
    } else {
        vec![IpAddr::V6(Ipv6Addr::UNSPECIFIED)]
    };

    let mut laddrs: Vec<SocketAddr> = vec![];

    for port in cli.ports.clone() {
        for ip in &interfaces {
            laddrs.push(SocketAddr::new(*ip, port));
        }
    }

    Ok(laddrs)
}

async fn run_n3() -> Result<()> {
    let cli = Cli::parse();

    if cli.debug {
        pretty_env_logger::try_init_timed().map_err(Error::other)?;
    }

    match cli.commands {
        Commands::Redirect { target } => {
            run_static_redirect(cli, target).await?;
        }
    }

    Ok(())
}

async fn run_static_redirect(cli: Cli, target: SocketAddr) -> Result<()> {
    let laddrs = parse_laddrs(&cli)?;

    let protos = cli
        .protos
        .iter()
        .map(|proto| proto.as_bytes())
        .collect::<Vec<_>>();

    N3::new(target)
        .quic_server(|quic_server| {
            quic_server
                .verify_peer(cli.verify_peer.is_some())
                .quiche_config(|config| {
                    config.set_initial_max_data(10_000_000);
                    // config.set_initial_max_stream_data_bidi_local(cli.max_stream_data_bidi);
                    config.set_initial_max_stream_data_bidi_remote(cli.max_stream_data_bidi);
                    config.set_initial_max_streams_bidi(cli.max_streams);
                    config.set_max_idle_timeout(cli.max_idle_timeout);

                    config
                        .load_cert_chain_from_pem_file(cli.cert.to_str().unwrap())
                        .map_err(|err| {
                            Error::new(
                                ErrorKind::NotFound,
                                format!(
                                    "Unable to load certificate chain file {:?}, {}",
                                    cli.cert, err
                                ),
                            )
                        })?;

                    config
                        .load_priv_key_from_pem_file(cli.key.to_str().unwrap())
                        .map_err(|err| {
                            Error::new(
                                ErrorKind::NotFound,
                                format!("Unable to load key file {:?}, {}", cli.cert, err),
                            )
                        })?;

                    if let Some(ca) = &cli.verify_peer {
                        config
                            .load_verify_locations_from_file(ca.to_str().unwrap())
                            .map_err(|err| {
                                Error::new(
                                    ErrorKind::NotFound,
                                    format!("Unable to trusted CA file {:?}, {}", cli.cert, err),
                                )
                            })?;
                    }

                    config.set_application_protos(&protos).map_err(|err| {
                        Error::new(
                            ErrorKind::InvalidInput,
                            format!(
                                "failed to set application protos as {:?}, {}",
                                cli.protos, err
                            ),
                        )
                    })?;

                    Ok(())
                })
        })
        .bind(laddrs.as_slice())
        .await
}

fn main() {
    if let Err(err) = block_on(run_n3()) {
        ceprintln!("<s><r>error:</r></s> {}", err)
    }
}
