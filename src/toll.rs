//! `hanvil toll`: an x402 payment rail on the chain this process owns.
//!
//! x402 settles a payment as a `CryptoTransfer` the client partially signs and the facilitator
//! co-signs and submits as fee payer. Every way that can fail — a bad partial signature, the
//! wrong fee payer, a replayed payment header — is refused by the node *before consensus*, and
//! on Hedera a pre-consensus refusal writes no record, no receipt and no mirror row anywhere.
//! x402 reports `transaction_failed` and nothing else exists to look up.
//!
//! This boots the node, hands the rail three of its predefined accounts, and supervises it, so
//! the loop that develops an x402 service costs no HBAR and no network — and every refusal is
//! readable from `hanvil_rejections`, because this process is the node.
//!
//! The rail is TypeScript: `@x402/hedera` is the `exact` scheme for Hedera, and a Rust
//! implementation would be a second copy of its wire format. So this spawns it, the way
//! `hanvil run` spawns an agent CLI and `npx`. The node itself still opens no outbound socket.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::cli::{NodeArgs, TollArgs};
use crate::harness::chain::LocalChain;
use crate::harness::devserver;
use crate::node::{self, Node};
use crate::state::{self, Clock, EntityId};

/// Where the bundled rail lives, relative to the working directory.
const DEFAULT_DIR: &str = "examples/toll";

/// What starts it. `package.json` orders the facilitator before the service, which matters:
/// the service reads the facilitator's `/supported` before it can quote a price.
const DEFAULT_COMMAND: &str = "yarn rail";

/// How long the rail has to answer on its service port. `yarn` plus two `tsx` starts.
const READY_TIMEOUT: Duration = Duration::from_secs(120);

/// How many predefined accounts the rail needs: a payer, a destination, and a fee payer.
const ACCOUNTS_NEEDED: usize = 3;

/// How often the supervised rail is checked for having exited.
const RAIL_POLL: Duration = Duration::from_millis(250);

/// What can stop the rail from coming up.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// The node could not boot.
    #[error(transparent)]
    Node(#[from] node::Error),

    /// The directory named does not hold a rail.
    #[error(
        "{path} does not look like a toll rail: no package.json. `hanvil toll [DIR]` serves the \
         rail in DIR, and defaults to {DEFAULT_DIR} under the working directory."
    )]
    NotARail {
        /// The directory that was checked.
        path: PathBuf,
    },

    /// Fewer predefined accounts than the rail needs.
    #[error(
        "the chain has {found} predefined ECDSA account(s) with keys and the rail needs \
         {ACCOUNTS_NEEDED}: a payer, a destination, and a fee payer for the facilitator. \
         Raise --accounts."
    )]
    NotEnoughAccounts {
        /// How many were found.
        found: usize,
    },

    /// The rail exited, or never answered.
    #[error("starting the rail with `{command}` in {dir}: {source}")]
    Rail {
        /// The command that was run.
        command: String,
        /// Where it ran.
        dir: PathBuf,
        /// Why it failed.
        #[source]
        source: devserver::Error,
    },

    /// The rail exited on its own while it was being served.
    #[error("the rail exited; `{command}` is no longer running")]
    RailExited {
        /// The command that was being supervised.
        command: String,
    },
}

/// One account the rail is given, and the role it plays in a settlement.
struct Role {
    id: EntityId,
    private_key_hex: String,
}

/// Boot the node, start the rail, print what it is, and hold both until ctrl-c.
pub(crate) async fn run(args: TollArgs, node_args: NodeArgs) -> Result<(), Error> {
    let dir = args
        .dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DIR));
    if !dir.join("package.json").is_file() {
        return Err(Error::NotARail { path: dir });
    }

    let clock: Arc<dyn Clock> = Arc::new(state::time::SystemClock);
    let chain = node::load_or_genesis(&node_args, clock.as_ref())?;
    let node = Node::boot(&node_args, chain, clock).await?;
    let local = LocalChain {
        rpc_url: format!("http://{}", node.rpc.local_addr),
        mirror_url: format!("http://{}", node.mirror.local_addr),
        grpc_url: node.grpc.local_addr.to_string(),
        chain_id: node.shared.read().chain_id(),
    };

    let roles = match roles(&node) {
        Ok(roles) => roles,
        Err(error) => {
            node.shutdown();
            return Err(error);
        }
    };

    let command = args
        .command
        .clone()
        .unwrap_or_else(|| DEFAULT_COMMAND.to_string());
    let config = devserver::Config {
        command: command.clone(),
        configured_url: format!("http://127.0.0.1:{}", args.service_port),
        timeout: READY_TIMEOUT,
    };
    let env = env(&local, &roles, &args);

    let mut session = match devserver::start(&dir, &config, "toll", &env).await {
        Ok(session) => session,
        Err(source) => {
            node.shutdown();
            return Err(Error::Rail {
                command,
                dir,
                source,
            });
        }
    };

    banner(&args, &local, &roles, &session.url);

    // Serve until the rail dies. An interrupt drops this future instead; `dispatch` then stops
    // every process group that was started, the way it does for `hanvil run`.
    while session.is_alive() {
        tokio::time::sleep(RAIL_POLL).await;
    }
    session.stop().await;
    node.shutdown();
    Err(Error::RailExited { command })
}

/// The payer, the destination and the facilitator, taken from the chain's predefined ECDSA
/// accounts in id order so two runs on the same genesis hand out the same three.
fn roles(node: &Node) -> Result<[Role; ACCOUNTS_NEEDED], Error> {
    let chain = node.shared.read();
    let groups = chain.accounts_by_group();
    let ecdsa = groups
        .first()
        .map(|(_, accounts)| accounts.as_slice())
        .unwrap_or_default();
    if ecdsa.len() < ACCOUNTS_NEEDED {
        return Err(Error::NotEnoughAccounts { found: ecdsa.len() });
    }
    let mut taken = ecdsa.iter().take(ACCOUNTS_NEEDED).map(|account| Role {
        id: account.id,
        // `accounts_by_group` only returns accounts that have one.
        private_key_hex: account.private_key_hex.clone().unwrap_or_default(),
    });
    let (payer, pay_to, facilitator) = (taken.next(), taken.next(), taken.next());
    match (payer, pay_to, facilitator) {
        (Some(payer), Some(pay_to), Some(facilitator)) => Ok([payer, pay_to, facilitator]),
        _ => Err(Error::NotEnoughAccounts { found: ecdsa.len() }),
    }
}

/// What every child of the rail sees. `HANVIL_*` and `HEDERA_NETWORK=local` come from the
/// chain; the rest is what the x402 halves need and would otherwise be typed out by hand.
fn env(
    local: &LocalChain,
    roles: &[Role; ACCOUNTS_NEEDED],
    args: &TollArgs,
) -> BTreeMap<String, String> {
    let [payer, pay_to, facilitator] = roles;
    let mut env = local.env();
    for (name, value) in [
        ("HEDERA_ACCOUNT_ID", payer.id.to_string()),
        ("HEDERA_PRIVATE_KEY", hex_key(&payer.private_key_hex)),
        ("PAY_TO", pay_to.id.to_string()),
        ("FACILITATOR_ACCOUNT_ID", facilitator.id.to_string()),
        (
            "FACILITATOR_PRIVATE_KEY",
            hex_key(&facilitator.private_key_hex),
        ),
        ("FACILITATOR_PORT", args.facilitator_port.to_string()),
        ("PORT", args.service_port.to_string()),
        ("TOLL_PRICE_TINYBARS", args.price.to_string()),
    ] {
        env.insert(name.to_string(), value);
    }
    env
}

/// `PrivateKey.fromStringECDSA` takes either form; the banner prints `0x`, so pass `0x`.
fn hex_key(hex: &str) -> String {
    if hex.starts_with("0x") {
        hex.to_string()
    } else {
        format!("0x{hex}")
    }
}

fn banner(args: &TollArgs, local: &LocalChain, roles: &[Role; ACCOUNTS_NEEDED], service_url: &str) {
    let [payer, pay_to, facilitator] = roles;
    println!();
    println!(
        "x402 facilitator  http://127.0.0.1:{}",
        args.facilitator_port
    );
    println!("Service           {service_url}");
    println!(
        "Settles on        the in-process chain ({})",
        local.grpc_url
    );
    println!("Price             {} tinybar per call", args.price);
    println!();
    println!("feePayer          {facilitator}  the facilitator submits and pays the fee");
    println!("payer             {payer}  the account a call is charged to");
    println!("payTo             {pay_to}  where a settled toll lands");
    println!();
    println!("To pay from another terminal:");
    println!(
        "  cd {dir}",
        dir = args
            .dir
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_DIR))
            .display()
    );
    println!(
        "  export HANVIL_GRPC_URL={} HANVIL_MIRROR_URL={}",
        local.grpc_url, local.mirror_url
    );
    println!(
        "  export HEDERA_ACCOUNT_ID={} HEDERA_PRIVATE_KEY={} PAY_TO={}",
        payer.id,
        hex_key(&payer.private_key_hex),
        pay_to.id
    );
    println!("  yarn pay        # one paid call");
    println!("  yarn replay     # the same payment replayed, and the refusal no mirror has");
    println!();
    println!("A refused settlement leaves no record on any Hedera network. This one keeps them:");
    println!(
        "  curl -s -X POST {} -H 'content-type: application/json' \\",
        local.rpc_url
    );
    println!(
        "    -d '{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"hanvil_rejections\",\"params\":[]}}'"
    );
    println!();
}

impl std::fmt::Display for Role {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.id)
    }
}
