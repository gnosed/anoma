mod events;
pub mod protocol;
pub mod rpc;
mod shell;
mod shims;
pub mod storage;
pub mod tendermint_node;

use std::convert::{TryFrom, TryInto};
use std::env;
use std::path::PathBuf;
use std::str::FromStr;

use anoma::types::chain::ChainId;
use anoma::types::storage::BlockHash;
use futures::TryFutureExt;
use tendermint_proto::abci::CheckTxType;
use tower::ServiceBuilder;
use tower_abci::{response, split, Server};

use crate::node::ledger::shell::{Error, MempoolTxType, Shell};
use crate::node::ledger::shims::abcipp_shim::AbcippShim;
use crate::node::ledger::shims::abcipp_shim_types::shim::{Request, Response};
use crate::{cli, config, wasm_loader};

/// Env. var to set a number of tokio RT worker threads
const ENV_VAR_ASYNC_WORKER_THREADS: &str = "ANOMA_ASYNC_WORKER_THREADS";

// Until ABCI++ is ready, the shim provides the service implementation.
// We will add this part back in once the shim is no longer needed.
//```
// impl Service<Request> for Shell {
//     type Error = Error;
//     type Future =
//         Pin<Box<dyn Future<Output = Result<Response, BoxError>> + Send +
// 'static>>;    type Response = Response;
//
//     fn poll_ready(
//         &mut self,
//         _cx: &mut Context<'_>,
//     ) -> Poll<Result<(), Self::Error>> {
//         Poll::Ready(Ok(()))
//     }
//```

impl Shell {
    fn call(&mut self, req: Request) -> Result<Response, Error> {
        match req {
            Request::InitChain(init) => {
                self.init_chain(init).map(Response::InitChain)
            }
            Request::Info(_) => Ok(Response::Info(self.last_state())),
            Request::Query(query) => Ok(Response::Query(self.query(query))),
            Request::PrepareProposal(block) => {
                match (
                    BlockHash::try_from(&*block.hash),
                    block.header.expect("missing block's header").try_into(),
                ) {
                    (Ok(hash), Ok(header)) => {
                        let _ = self.prepare_proposal(
                            hash,
                            header,
                            block.byzantine_validators,
                        );
                    }
                    (Ok(_), Err(msg)) => {
                        tracing::error!("Unexpected block header {}", msg);
                    }
                    (err @ Err(_), _) => tracing::error!("{:#?}", err),
                };
                Ok(Response::PrepareProposal(Default::default()))
            }
            Request::VerifyHeader(_req) => {
                Ok(Response::VerifyHeader(self.verify_header(_req)))
            }
            Request::ProcessProposal(block) => {
                Ok(Response::ProcessProposal(self.process_proposal(block)))
            }
            Request::RevertProposal(_req) => {
                Ok(Response::RevertProposal(self.revert_proposal(_req)))
            }
            Request::ExtendVote(_req) => {
                Ok(Response::ExtendVote(self.extend_vote(_req)))
            }
            Request::VerifyVoteExtension(_req) => {
                Ok(Response::VerifyVoteExtension(Default::default()))
            }
            Request::FinalizeBlock(finalize) => {
                self.finalize_block(finalize).map(Response::FinalizeBlock)
            }
            Request::Commit(_) => Ok(Response::Commit(self.commit())),
            Request::Flush(_) => Ok(Response::Flush(Default::default())),
            Request::SetOption(_) => {
                Ok(Response::SetOption(Default::default()))
            }
            Request::Echo(msg) => Ok(Response::Echo(response::Echo {
                message: msg.message,
            })),
            Request::CheckTx(tx) => {
                let r#type = match CheckTxType::from_i32(tx.r#type)
                    .expect("received unexpected CheckTxType from ABCI")
                {
                    CheckTxType::New => MempoolTxType::NewTransaction,
                    CheckTxType::Recheck => MempoolTxType::RecheckTransaction,
                };
                Ok(Response::CheckTx(self.mempool_validate(&*tx.tx, r#type)))
            }
            Request::ListSnapshots(_) => {
                Ok(Response::ListSnapshots(Default::default()))
            }
            Request::OfferSnapshot(_) => {
                Ok(Response::OfferSnapshot(Default::default()))
            }
            Request::LoadSnapshotChunk(_) => {
                Ok(Response::LoadSnapshotChunk(Default::default()))
            }
            Request::ApplySnapshotChunk(_) => {
                Ok(Response::ApplySnapshotChunk(Default::default()))
            }
        }
    }
}

/// Run the ledger with an async runtime
pub fn run(config: config::Ledger, wasm_dir: PathBuf) {
    let logical_cores =
        if let Ok(num_str) = env::var(ENV_VAR_ASYNC_WORKER_THREADS) {
            match usize::from_str(&num_str) {
                Ok(num) => num,
                Err(_) => {
                    eprintln!(
                        "Invalid env. var {} value: {}. Expecting a `usize` \
                         number.",
                        ENV_VAR_ASYNC_WORKER_THREADS, num_str
                    );
                    cli::safe_exit(1)
                }
            }
        } else {
            // If not set, default to half of logical CPUs count
            num_cpus::get() / 2
        };

    // Start tokio runtime with the `run_aux` function
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(logical_cores)
        .thread_name("ledger-async-worker")
        // Enable time and I/O drivers
        .enable_all()
        .build()
        .unwrap()
        .block_on(run_aux(config, wasm_dir));
}

/// Resets the tendermint_node state and removes database files
pub fn reset(config: config::Ledger) -> Result<(), shell::Error> {
    shell::reset(config)
}

/// Runs two concurrent tasks: A tendermint node, a shell which contains an ABCI
/// server for talking to the tendermint node. Both must be alive for correct
/// functioning.
async fn run_aux(config: config::Ledger, wasm_dir: PathBuf) {
    // Prefetch needed wasm artifacts
    wasm_loader::pre_fetch_wasm(&wasm_dir).await;

    let tendermint_dir = config.tendermint_dir();
    let ledger_address = config.shell.ledger_address.to_string();
    let chain_id = config.chain_id.clone();
    let genesis_time = config
        .genesis_time
        .clone()
        .try_into()
        .expect("expected RFC3339 genesis_time");
    let tendermint_config = config.tendermint.clone();

    // Channel for signalling shut down from the shell or from Tendermint
    let (abort_send, mut abort_recv) =
        tokio::sync::mpsc::unbounded_channel::<&'static str>();

    // Channel for signalling shut down to Tendermint process
    let (tm_abort_send, tm_abort_recv) =
        tokio::sync::oneshot::channel::<tokio::sync::oneshot::Sender<()>>();

    // Start Tendermint node
    let abort_send_for_tm = abort_send.clone();
    let tendermint_node = tokio::spawn(async move {
        // On panic or exit, the `Drop` of `AbortSender` will send abort message
        let aborter = Aborter {
            sender: abort_send_for_tm,
            who: "Tendermint",
        };

        let res = tendermint_node::run(
            tendermint_dir,
            chain_id,
            genesis_time,
            ledger_address,
            tendermint_config,
            tm_abort_recv,
        )
        .map_err(Error::Tendermint)
        .await;
        tracing::info!("Tendermint node is no longer running.");

        drop(aborter);
        res
    });

    // Start the shell in a ABCI server
    let shell = tokio::spawn(async move {
        // On panic or exit, the `Drop` of `AbortSender` will send abort message
        let aborter = Aborter {
            sender: abort_send,
            who: "Shell",
        };

        let res = run_shell(config.chain_id, config.shell, wasm_dir).await;

        drop(aborter);
        res
    });

    tracing::info!("Anoma ledger node started.");

    // Wait for interrupt signal or abort message
    tokio::select! {
        signal = tokio::signal::ctrl_c() => {
            match signal {
                Ok(()) => tracing::info!("Received interrupt signal, exiting..."),
                Err(err) => tracing::error!("Failed to listen for CTRL+C signal: {}", err),
            }
        },
        msg = abort_recv.recv() => {
            // When the msg is `None`, there are no more abort senders, so both
            // Tendermint and the shell must have already exited
            if let Some(who) = msg {
                 tracing::info!("{} has exited, shutting down...", who);
            }
        }
    };

    // Abort the shell task
    shell.abort();

    // Shutdown tendermint_node via a message to ensure that the child process
    // is properly cleaned-up.
    let (tm_abort_resp_send, tm_abort_resp_recv) =
        tokio::sync::oneshot::channel::<()>();
    // Ask to shutdown tendermint node cleanly. Ignore error, which can happen
    // if the tendermint_node task has already finished.
    if let Ok(()) = tm_abort_send.send(tm_abort_resp_send) {
        match tm_abort_resp_recv.await {
            Ok(()) => {}
            Err(err) => {
                tracing::error!(
                    "Failed to receive a response from tendermint: {}",
                    err
                );
            }
        }
    }

    let res = tokio::try_join!(tendermint_node, shell);
    match res {
        Ok((tendermint_res, shell_res)) => {
            if let Err(err) = tendermint_res {
                tracing::error!("Tendermint error: {}", err);
            }
            if let Err(err) = shell_res {
                tracing::error!("Shell error: {}", err);
            }
        }
        Err(err) => {
            // Ignore cancellation errors
            if !err.is_cancelled() {
                tracing::error!("Ledger error: {}", err);
            }
        }
    }
    tracing::info!("Anoma ledger node has shut down.");
}

/// Runs the an asynchronous ABCI server with four sub-components for consensus,
/// mempool, snapshot, and info.
async fn run_shell(
    chain_id: ChainId,
    config: config::Shell,
    wasm_dir: PathBuf,
) -> shell::Result<()> {
    // Construct our ABCI application.
    let db_dir = config.db_dir(&chain_id);
    let service = AbcippShim::new(config.base_dir, db_dir, chain_id, wasm_dir);

    // Split it into components.
    let (consensus, mempool, snapshot, info) = split::service(service, 5);

    // Hand those components to the ABCI server, but customize request behavior
    // for each category
    let server = Server::builder()
        .consensus(consensus)
        .snapshot(snapshot)
        .mempool(
            ServiceBuilder::new()
                .load_shed()
                .buffer(10)
                .service(mempool),
        )
        .info(
            ServiceBuilder::new()
                .load_shed()
                .buffer(100)
                .rate_limit(50, std::time::Duration::from_secs(1))
                .service(info),
        )
        .finish()
        .unwrap();

    // Run the server with the shell
    server
        .listen(config.ledger_address)
        .await
        .map_err(|err| Error::TowerServer(err.to_string()))
}

/// A panic-proof handle for aborting a future. Will abort during stack
/// unwinding and its drop method sends abort message with `who` inside it.
struct Aborter {
    sender: tokio::sync::mpsc::UnboundedSender<&'static str>,
    who: &'static str,
}

impl Drop for Aborter {
    fn drop(&mut self) {
        // Send abort message, ignore result
        let _ = self.sender.send(self.who);
    }
}
