//! The HAPI services Hanvil does not emulate.
//!
//! Registering them is the whole point of this file. An unregistered service is not routed, so
//! tonic answers a call to one with gRPC status 12 `UNIMPLEMENTED` and an empty message —
//! `@hiero-ledger/sdk` reports that as `Error: 12 UNIMPLEMENTED:`, a transport failure with
//! nothing in it. Routed, the same call answers `NOT_SUPPORTED` (`response_code.proto:98`), which
//! the SDK reports as a precheck the network refused, in the shape every other refusal takes.

use tonic::{Request, Response, Status as GrpcStatus};

use super::{Node, proto};

/// Implement one whole service as a refusal.
///
/// A transaction body Hanvil does not execute answers `NOT_SUPPORTED` at precheck: no record, no
/// fee. A query answers a gRPC status naming what was asked for, because `Query` carries no
/// precheck code outside the response variant it would have to build.
///
/// The `#[tonic::async_trait]` attribute is inside the macro rather than on each `impl`: applied
/// outside, it would see an unexpanded `macro_rules!` invocation instead of the `async fn`s it
/// has to desugar, and every method would mismatch the trait's lifetimes.
macro_rules! refuse {
    (
        $service:path,
        transactions: [$($transaction:ident),* $(,)?],
        queries: [$($query:ident => $asked:literal),* $(,)?] $(,)?
    ) => {
        #[tonic::async_trait]
        impl $service for Node {
            $(
                async fn $transaction(
                    &self,
                    _: Request<proto::Transaction>,
                ) -> Result<Response<proto::TransactionResponse>, GrpcStatus> {
                    Ok(Response::new(Node::unsupported()))
                }
            )*
            $(
                async fn $query(
                    &self,
                    _: Request<proto::Query>,
                ) -> Result<Response<proto::Response>, GrpcStatus> {
                    Err(Node::unsupported_query($asked))
                }
            )*
        }
    };
}

refuse!(
    proto::file_service_server::FileService,
    transactions: [
        create_file,
        update_file,
        delete_file,
        append_content,
        system_delete,
        system_undelete,
    ],
    queries: [
        get_file_content => "FileGetContents",
        get_file_info => "FileGetInfo",
    ],
);

refuse!(
    proto::token_service_server::TokenService,
    transactions: [
        create_token,
        update_token,
        mint_token,
        burn_token,
        delete_token,
        wipe_token_account,
        freeze_token_account,
        unfreeze_token_account,
        grant_kyc_to_token_account,
        revoke_kyc_from_token_account,
        associate_tokens,
        dissociate_tokens,
        update_token_fee_schedule,
        pause_token,
        unpause_token,
        update_nfts,
        reject_token,
        airdrop_tokens,
        cancel_airdrop,
        claim_airdrop,
    ],
    queries: [
        get_token_info => "TokenGetInfo",
        get_token_nft_info => "TokenGetNftInfo",
    ],
);

refuse!(
    proto::schedule_service_server::ScheduleService,
    transactions: [create_schedule, sign_schedule, delete_schedule],
    queries: [get_schedule_info => "ScheduleGetInfo"],
);

refuse!(
    proto::freeze_service_server::FreezeService,
    transactions: [freeze],
    queries: [],
);

refuse!(
    proto::util_service_server::UtilService,
    transactions: [prng, atomic_batch],
    queries: [],
);

refuse!(
    proto::address_book_service_server::AddressBookService,
    transactions: [
        create_node,
        delete_node,
        update_node,
        create_registered_node,
        delete_registered_node,
        update_registered_node,
    ],
    queries: [],
);
