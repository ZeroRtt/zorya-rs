use std::{
    io::{Error, ErrorKind, Result},
    net::{IpAddr, SocketAddr},
    ops::Range,
    path::PathBuf,
};

use clap::{Parser, Subcommand};
use color_print::ceprintln;
use futures::executor::block_on;
use n3agent::Agent;

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
    /// Set agent proto list.
    #[arg(long, value_name = "PROTO_LIST", default_values_t = ["n3".to_string()])]
    protos: Vec<String>,

    /// Specify n3 server listening address.
    #[arg(short = 'i', long, value_name = "ADDR")]
    n3_ip: IpAddr,

    /// Specify the n3 server listening port range.
    #[arg(short = 'p', long, value_name = "PORT", value_parser=parse_port_range)]
    n3_port_range: Range<u16>,

    /// Configure the certificate chain file(PEM).
    #[arg(short, long, value_name = "PEM_FILE")]
    cert: Option<PathBuf>,

    /// Configure the private chain file(PEM).
    #[arg(short, long, value_name = "PEM_FILE", default_value = "n3.key")]
    key: PathBuf,

    /// Sets the initial_max_stream_data_bidi_remote transport parameter.
    ///
    /// When set to a non-zero value quiche will only allow at most v bytes of incoming stream data
    /// to be buffered for each locally-initiated bidirectional stream (that is, data that is not
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

    /// Sets the `max_ack_delay` transport parameter, in milliseconds.
    #[arg(long, value_name = "SIZE", default_value_t = 50)]
    max_ack_delay: u64,

    /// Sets the quiche `initial_max_streams_bidi` transport parameter.
    ///
    /// When set to a non-zero value quiche will only allow v number of concurrent remotely-initiated bidirectional
    /// streams to be open at any given time and will increase the limit automatically as streams are completed.
    #[arg(long, value_name = "STREAMS", default_value_t = 100)]
    max_streams: u64,

    /// Debug mode, print verbose output informations.
    #[arg(short, long, default_value_t = false, action)]
    debug: bool,

    #[command(subcommand)]
    commands: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start a agent service.
    Listen {
        /// Specify the redirect target address
        target: Option<SocketAddr>,
    },
}

fn parse_n3_addrs(cli: &Cli) -> Result<Vec<SocketAddr>> {
    let mut laddrs: Vec<SocketAddr> = vec![];

    for port in cli.n3_port_range.clone() {
        laddrs.push(SocketAddr::new(cli.n3_ip, port));
    }

    Ok(laddrs)
}

async fn run_agent(cli: Cli, laddr: SocketAddr) -> Result<()> {
    let n3_addrs = parse_n3_addrs(&cli)?;

    if n3_addrs.is_empty() {
        return Err(Error::new(ErrorKind::InvalidInput, "`n3_addrs` ."));
    }

    let protos = cli
        .protos
        .iter()
        .map(|proto| proto.as_bytes())
        .collect::<Vec<_>>();

    Agent::new(n3_addrs.as_slice())
        .connector(|connector| {
            connector.quiche_config(|config| {
                config.set_initial_max_data(cli.max_stream_data_bidi * cli.max_streams);
                config.set_initial_max_stream_data_bidi_local(cli.max_stream_data_bidi);
                config.set_initial_max_streams_bidi(cli.max_streams);
                config.set_max_idle_timeout(cli.max_idle_timeout);
                config.set_max_ack_delay(cli.max_ack_delay);

                if let Some(cert) = &cli.cert {
                    config
                        .load_cert_chain_from_pem_file(cert.to_str().unwrap())
                        .map_err(|err| {
                            Error::new(
                                ErrorKind::NotFound,
                                format!(
                                    "Unable to load certificate chain file {:?}, {}",
                                    cli.cert, err
                                ),
                            )
                        })?;
                }

                config
                    .load_priv_key_from_pem_file(cli.key.to_str().unwrap())
                    .map_err(|err| {
                        Error::new(
                            ErrorKind::NotFound,
                            format!("Unable to load key file {:?}, {}", cli.key, err),
                        )
                    })?;

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
        .bind(laddr)
        .await
}

async fn run_n3_agent() -> Result<()> {
    let cli = Cli::parse();

    if cli.debug {
        pretty_env_logger::try_init_timed().map_err(Error::other)?;
    }

    match cli.commands {
        Commands::Listen { target } => {
            run_agent(
                cli,
                target.unwrap_or("[::]:1812".parse().map_err(Error::other)?),
            )
            .await?;
        }
    }

    Ok(())
}

fn main() {
    if let Err(err) = block_on(run_n3_agent()) {
        ceprintln!("<s><r>error:</r></s> {}", err)
    }
}
