use std::ffi::OsStr;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use std::sync::Once;
use std::{env, fs, mem, thread, time};

use anoma::types::address::Address;
use anoma::types::chain::ChainId;
use anoma::types::key::ed25519::Keypair;
use anoma_apps::client::utils;
use anoma_apps::config::genesis::genesis_config::{self, GenesisConfig};
use anoma_apps::{config, wallet, wasm_loader};
use assert_cmd::assert::OutputAssertExt;
use color_eyre::eyre::Result;
use color_eyre::owo_colors::OwoColorize;
use escargot::CargoBuild;
use eyre::eyre;
use rexpect::process::wait::WaitStatus;
use rexpect::session::{spawn_command, PtySession};
use tempfile::{tempdir, TempDir};

/// For `color_eyre::install`, which fails if called more than once in the same
/// process
static INIT: Once = Once::new();

const APPS_PACKAGE: &str = "anoma_apps";

/// Env. var for running E2E tests in debug mode
const ENV_VAR_DEBUG: &str = "ANOMA_E2E_DEBUG";

/// Env. var for keeping temporary files created by the E2E tests
const ENV_VAR_KEEP_TEMP: &str = "ANOMA_E2E_KEEP_TEMP";

/// The E2E tests genesis config source.
/// This file must contain a single validator with alias "validator-0".
/// To add more validators, use the [`add_validators`] function in the call to
/// setup the [`network`].
const SINGLE_NODE_NET_GENESIS: &str = "genesis/e2e-tests-single-node.toml";

/// An E2E test network.
#[derive(Debug)]
pub struct Network {
    chain_id: ChainId,
}

/// Add `num` validators to the genesis config.
/// INVARIANT: Do not call this function more than once on the same config.
pub fn add_validators(num: u8, mut genesis: GenesisConfig) -> GenesisConfig {
    let validator_0 = genesis.validator.get("validator-0").unwrap().clone();
    let net_address_0 =
        SocketAddr::from_str(validator_0.net_address.as_ref().unwrap())
            .unwrap();
    let net_address_port_0 = net_address_0.port();
    for ix in 0..num {
        let mut validator = validator_0.clone();
        let mut net_address = net_address_0;
        // 5 ports for each validator
        net_address.set_port(net_address_port_0 + 5 * (ix as u16 + 1));
        validator.net_address = Some(net_address.to_string());
        let name = format!("validator-{}", ix + 1);
        genesis.validator.insert(name, validator);
    }
    genesis
}

/// Setup a network with a single genesis validator node.
pub fn single_node_net() -> Result<Test> {
    network(|genesis| genesis)
}

/// Setup a configurable network.
pub fn network(
    update_genesis: impl Fn(GenesisConfig) -> GenesisConfig,
) -> Result<Test> {
    INIT.call_once(|| {
        if let Err(err) = color_eyre::install() {
            eprintln!("Failed setting up colorful error reports {}", err);
        }
    });
    let working_dir = working_dir();
    let base_dir = tempdir().unwrap();

    // Open the source genesis file
    let genesis = genesis_config::open_genesis_config(
        working_dir.join(SINGLE_NODE_NET_GENESIS),
    );

    // Run the provided function on it
    let mut genesis = update_genesis(genesis);

    // Update the WASM sha256 fields
    let checksums =
        wasm_loader::Checksums::read_checksums(working_dir.join("wasm"));
    genesis.wasm.iter_mut().for_each(|(name, config)| {
        // Find the sha256 from checksums.json
        let name = format!("{}.wasm", name);
        // Full name in format `{name}.{sha256}.wasm`
        let full_name = checksums.0.get(&name).unwrap();
        let hash = full_name
            .split_once(".")
            .unwrap()
            .1
            .split_once(".")
            .unwrap()
            .0;
        config.sha256 = genesis_config::HexString(hash.to_owned());
    });

    // Run `init-network` to generate the finalized genesis config, keys and
    // addresses
    let genesis_file = base_dir.path().join("e2e-test-genesis-src.toml");
    genesis_config::write_genesis_config(&genesis, &genesis_file);
    let genesis_path = genesis_file.to_string_lossy();

    let mut init_network = run_cmd(
        Bin::Client,
        [
            "utils",
            "init-network",
            "--unsafe-dont-encrypt",
            "--genesis-path",
            genesis_path.as_ref(),
            "--chain-prefix",
            "e2e-test",
            "--localhost",
            "--dont-archive",
        ],
        Some(5),
        &working_dir,
        &base_dir,
        format!("{}:{}", std::file!(), std::line!()),
    )?;

    // Get the generated chain_id` from result of the last command
    let (unread, matched) =
        init_network.exp_regex(r"Derived chain ID: .*\n")?;
    let chain_id_raw =
        matched.trim().split_once("Derived chain ID: ").unwrap().1;
    let chain_id = ChainId::from_str(chain_id_raw.trim())?;
    println!("'init-network' output: {}", unread);
    let net = Network { chain_id };

    // Move the "others" accounts wallet in the main base dir, so that we can
    // use them with `Who::NonValidator`
    let chain_dir = base_dir.path().join(net.chain_id.as_str());
    std::fs::rename(
        wallet::wallet_file(
            chain_dir
                .join(utils::NET_ACCOUNTS_DIR)
                .join(utils::NET_OTHER_ACCOUNTS_DIR),
        ),
        wallet::wallet_file(chain_dir),
    )
    .unwrap();

    Ok(Test {
        working_dir,
        base_dir,
        net,
    })
}

/// Anoma binaries
#[derive(Debug)]
pub enum Bin {
    Node,
    Client,
    Wallet,
}

#[derive(Debug)]
pub struct Test {
    pub working_dir: PathBuf,
    pub base_dir: TempDir,
    pub net: Network,
}

impl Drop for Test {
    fn drop(&mut self) {
        let keep_temp = match env::var(ENV_VAR_KEEP_TEMP) {
            Ok(val) => val.to_ascii_lowercase() != "false",
            _ => false,
        };
        if keep_temp {
            if cfg!(any(unix, target_os = "redox", target_os = "wasi")) {
                let path = mem::replace(&mut self.base_dir, tempdir().unwrap());
                println!(
                    "{}: \"{}\"",
                    "Keeping temporary directory at".underline().yellow(),
                    path.path().to_string_lossy()
                );
                mem::forget(path);
            } else {
                eprintln!(
                    "Setting {} is not supported on this platform",
                    ENV_VAR_KEEP_TEMP
                );
            }
        }
    }
}

// Internally used macros only for attaching source locations to commands
#[macro_use]
mod macros {
    /// Get an [`AnomaCmd`] to run an Anoma binary. By default, these will run
    /// in release mode. This can be disabled by setting environment
    /// variable `ANOMA_E2E_DEBUG=true`.
    /// On [`AnomaCmd`], you can then call e.g. `exp_string` or `exp_regex` to
    /// look for an expected output from the command.
    ///
    /// Arguments:
    /// - the test [`super::Test`]
    /// - which binary to run [`super::Bin`]
    /// - arguments, which implement `IntoIterator<Item = &str>`, e.g.
    ///   `&["cmd"]`
    /// - optional timeout in seconds `Option<u64>`
    ///
    /// This is a helper macro that adds file and line location to the
    /// [`super::run_cmd`] function call.
    #[macro_export]
    macro_rules! run {
        ($test:expr, $bin:expr, $args:expr, $timeout_sec:expr $(,)?) => {{
            // The file and line will expand to the location that invoked
            // `run_cmd!`
            let loc = format!("{}:{}", std::file!(), std::line!());
            $test.run_cmd($bin, $args, $timeout_sec, loc)
        }};
    }

    /// Get an [`AnomaCmd`] to run an Anoma binary. By default, these will run
    /// in release mode. This can be disabled by setting environment
    /// variable `ANOMA_E2E_DEBUG=true`.
    /// On [`AnomaCmd`], you can then call e.g. `exp_string` or `exp_regex` to
    /// look for an expected output from the command.
    ///
    /// Arguments:
    /// - the test [`super::Test`]
    /// - who to run this command as [`super::Who`]
    /// - which binary to run [`super::Bin`]
    /// - arguments, which implement `IntoIterator<item = &str>`, e.g.
    ///   `&["cmd"]`
    /// - optional timeout in seconds `Option<u64>`
    ///
    /// This is a helper macro that adds file and line location to the
    /// [`super::run_cmd`] function call.
    #[macro_export]
    macro_rules! run_as {
        (
            $test:expr,
            $who:expr,
            $bin:expr,
            $args:expr,
            $timeout_sec:expr $(,)?
        ) => {{
            // The file and line will expand to the location that invoked
            // `run_cmd!`
            let loc = format!("{}:{}", std::file!(), std::line!());
            $test.run_cmd_as($who, $bin, $args, $timeout_sec, loc)
        }};
    }
}

pub enum Who {
    // A non-validator
    NonValidator,
    // Genesis validator with a given index, starting from `0`
    Validator(u64),
}

impl Test {
    /// Use the `run!` macro instead of calling this method directly to get
    /// automatic source location reporting.
    ///
    /// Get an [`AnomaCmd`] to run an Anoma binary. By default, these will run
    /// in release mode. This can be disabled by setting environment
    /// variable `ANOMA_E2E_DEBUG=true`.
    pub fn run_cmd<I, S>(
        &self,
        bin: Bin,
        args: I,
        timeout_sec: Option<u64>,
        loc: String,
    ) -> Result<AnomaCmd>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.run_cmd_as(Who::NonValidator, bin, args, timeout_sec, loc)
    }

    /// Use the `run!` macro instead of calling this method directly to get
    /// automatic source location reporting.
    ///
    /// Get an [`AnomaCmd`] to run an Anoma binary. By default, these will run
    /// in release mode. This can be disabled by setting environment
    /// variable `ANOMA_E2E_DEBUG=true`.
    pub fn run_cmd_as<I, S>(
        &self,
        who: Who,
        bin: Bin,
        args: I,
        timeout_sec: Option<u64>,
        loc: String,
    ) -> Result<AnomaCmd>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let base_dir = match who {
            Who::NonValidator => self.base_dir.path().to_owned(),
            Who::Validator(index) => self
                .base_dir
                .path()
                .join(self.net.chain_id.as_str())
                .join(utils::NET_ACCOUNTS_DIR)
                .join(format!("validator-{}", index))
                .join(config::DEFAULT_BASE_DIR),
        };
        run_cmd(bin, args, timeout_sec, &self.working_dir, &base_dir, loc)
    }
}

/// A helper that should be ran on start of every e2e test case.
pub fn working_dir() -> PathBuf {
    let working_dir = fs::canonicalize("..").unwrap();
    // Check that tendermint is on $PATH
    Command::new("which").arg("tendermint").assert().success();
    working_dir
}

/// A command under test
pub struct AnomaCmd {
    pub session: PtySession,
}

impl AnomaCmd {
    /// Assert that the process exited with success
    pub fn assert_success(&self) {
        let status = self.session.process.wait().unwrap();
        assert_eq!(
            WaitStatus::Exited(self.session.process.child_pid, 0),
            status
        );
    }

    /// Wait until provided string is seen on stdout of child process.
    /// Return the yet unread output (without the matched string)
    ///
    /// Wrapper over the inner `PtySession`'s functions with custom error
    /// reporting.
    pub fn exp_string(&mut self, needle: &str) -> Result<String> {
        self.session
            .exp_string(needle)
            .map_err(|e| eyre!(format!("{}", e)))
    }

    /// Wait until provided regex is seen on stdout of child process.
    /// Return a tuple:
    /// 1. the yet unread output
    /// 2. the matched regex
    ///
    /// Wrapper over the inner `PtySession`'s functions with custom error
    /// reporting.
    pub fn exp_regex(&mut self, regex: &str) -> Result<(String, String)> {
        self.session
            .exp_regex(regex)
            .map_err(|e| eyre!(format!("{}", e)))
    }

    /// Wait until we see EOF (i.e. child process has terminated)
    /// Return all the yet unread output
    ///
    /// Wrapper over the inner `PtySession`'s functions with custom error
    /// reporting.
    #[allow(dead_code)]
    pub fn exp_eof(&mut self) -> Result<String> {
        self.session.exp_eof().map_err(|e| eyre!(format!("{}", e)))
    }

    /// Send a control code to the running process and consume resulting output
    /// line (which is empty because echo is off)
    ///
    /// E.g. `send_control('c')` sends ctrl-c. Upper/smaller case does not
    /// matter.
    ///
    /// Wrapper over the inner `PtySession`'s functions with custom error
    /// reporting.
    pub fn send_control(&mut self, c: char) -> Result<()> {
        self.session
            .send_control(c)
            .map_err(|e| eyre!(format!("{}", e)))
    }

    /// send line to repl (and flush output) and then, if echo_on=true wait for
    /// the input to appear.
    /// Return: number of bytes written
    ///
    /// Wrapper over the inner `PtySession`'s functions with custom error
    /// reporting.
    pub fn send_line(&mut self, line: &str) -> Result<usize> {
        self.session
            .send_line(line)
            .map_err(|e| eyre!(format!("{}", e)))
    }
}

impl Drop for AnomaCmd {
    fn drop(&mut self) {
        // Clean up the process, if its still running
        let _ = self.session.process.exit();
    }
}

/// Get a [`Command`] to run an Anoma binary. By default, these will run in
/// release mode. This can be disabled by setting environment variable
/// `ANOMA_E2E_DEBUG=true`.
pub fn run_cmd<I, S>(
    bin: Bin,
    args: I,
    timeout_sec: Option<u64>,
    working_dir: impl AsRef<Path>,
    base_dir: impl AsRef<Path>,
    loc: String,
) -> Result<AnomaCmd>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    // Root cargo workspace manifest path
    let manifest_path = working_dir.as_ref().join("Cargo.toml");
    let bin_name = match bin {
        Bin::Node => "anoman",
        Bin::Client => "anomac",
        Bin::Wallet => "anomaw",
    };
    // Allow to run in debug
    let run_debug = match env::var(ENV_VAR_DEBUG) {
        Ok(val) => val.to_ascii_lowercase() != "false",
        _ => false,
    };
    let cmd = CargoBuild::new()
        .package(APPS_PACKAGE)
        .manifest_path(manifest_path)
        .bin(bin_name);
    let cmd = if run_debug {
        cmd
    } else {
        // Use the same build settings as `make build-release`
        cmd.release()
    };
    let mut cmd = cmd.run().unwrap().command();
    cmd.env("ANOMA_LOG", "anoma=debug")
        // Explicitly disable dev, in case it's enabled when a test is invoked
        .env("ANOMA_DEV", "false")
        .current_dir(working_dir)
        .args(&["--base-dir", &base_dir.as_ref().to_string_lossy()])
        .args(args);
    let cmd_str = format!("{:?}", cmd);

    let timeout_ms = timeout_sec.map(|sec| sec * 1_000);
    println!("{}: {}", "Running".underline().green(), cmd_str);
    let mut session = spawn_command(cmd, timeout_ms).map_err(|e| {
        eyre!(
            "\n\n{}: {}\n{}: {}\n{}: {}",
            "Failed to run".underline().red(),
            cmd_str,
            "Location".underline().red(),
            loc,
            "Error".underline().red(),
            e
        )
    })?;

    if let Bin::Node = &bin {
        // When running a node command, we need to wait a bit before checking
        // status
        sleep(1);

        // If the command failed, try print out its output
        if let Some(rexpect::process::wait::WaitStatus::Exited(_, result)) =
            session.process.status()
        {
            if result != 0 {
                return Err(eyre!(
                    "\n\n{}: {}\n{}: {} \n\n{}: {}",
                    "Failed to run".underline().red(),
                    cmd_str,
                    "Location".underline().red(),
                    loc,
                    "Output".underline().red(),
                    session.exp_eof().unwrap_or_else(|err| format!(
                        "No output found, error: {}",
                        err
                    ))
                ));
            }
        }
    }
    Ok(AnomaCmd { session })
}

/// Sleep for given `seconds`.
pub fn sleep(seconds: u64) {
    thread::sleep(time::Duration::from_secs(seconds));
}

/// Find the address of an account by its alias from the wallet
pub fn find_address(test: &Test, alias: impl AsRef<str>) -> Result<Address> {
    let mut find = run!(
        test,
        Bin::Wallet,
        &["address", "find", "--alias", alias.as_ref()],
        Some(1)
    )?;
    let (unread, matched) = find.exp_regex("Found address .*\n")?;
    let address = matched.trim().rsplit_once(" ").unwrap().1;
    Address::from_str(address).map_err(|e| {
        eyre!(format!(
            "Address: {} parsed from {}, Error: {}\n\nOutput: {}",
            address, matched, e, unread
        ))
    })
}

/// Find the address of an account by its alias from the wallet
pub fn find_keypair(test: &Test, alias: impl AsRef<str>) -> Result<Keypair> {
    let mut find = run!(
        test,
        Bin::Wallet,
        &[
            "key",
            "find",
            "--alias",
            alias.as_ref(),
            "--unsafe-show-secret"
        ],
        Some(1)
    )?;
    let (_unread, matched) = find.exp_regex("Public key: .*\n")?;
    let pk = matched.trim().rsplit_once(" ").unwrap().1;
    let (unread, matched) = find.exp_regex("Secret key: .*\n")?;
    let sk = matched.trim().rsplit_once(" ").unwrap().1;
    let key = format!("{}{}", sk, pk);
    Keypair::from_str(&key).map_err(|e| {
        eyre!(format!(
            "Key: {} parsed from {}, Error: {}\n\nOutput: {}",
            key, matched, e, unread
        ))
    })
}

#[allow(dead_code)]
pub mod constants {
    use std::fs;
    use std::path::PathBuf;

    // User addresses aliases
    pub const ALBERT: &str = "Albert";
    pub const ALBERT_KEY: &str = "Albert-key";
    pub const BERTHA: &str = "Bertha";
    pub const BERTHA_KEY: &str = "Bertha-key";
    pub const CHRISTEL: &str = "Christel";
    pub const CHRISTEL_KEY: &str = "Christel-key";
    pub const DAEWON: &str = "Daewon";

    // Fungible token addresses
    pub const XAN: &str = "XAN";
    pub const BTC: &str = "BTC";
    pub const ETH: &str = "ETH";
    pub const DOT: &str = "DOT";

    // Bite-sized tokens
    pub const SCHNITZEL: &str = "Schnitzel";
    pub const APFEL: &str = "Apfel";
    pub const KARTOFFEL: &str = "Kartoffel";

    // Paths to the WASMs used for tests
    pub const TX_TRANSFER_WASM: &str = "wasm/tx_transfer.wasm";
    pub const VP_USER_WASM: &str = "wasm/vp_user.wasm";
    pub const TX_NO_OP_WASM: &str = "wasm_for_tests/tx_no_op.wasm";
    pub const VP_ALWAYS_TRUE_WASM: &str = "wasm_for_tests/vp_always_true.wasm";
    pub const VP_ALWAYS_FALSE_WASM: &str =
        "wasm_for_tests/vp_always_false.wasm";
    pub const TX_MINT_TOKENS_WASM: &str = "wasm_for_tests/tx_mint_tokens.wasm";

    /// Find the absolute path to one of the WASM files above
    pub fn wasm_abs_path(file_name: &str) -> PathBuf {
        let working_dir = fs::canonicalize("..").unwrap();
        working_dir.join(file_name)
    }
}
