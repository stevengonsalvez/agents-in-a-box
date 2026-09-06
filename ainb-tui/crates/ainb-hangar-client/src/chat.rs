//! The part-2 chat and copilot calls: channels, confirm cards, activity.
//!
//! A separate FILE, not more methods on the list in `lib.rs`, deliberately. Two
//! agents landed part 2 in parallel (the daemon/CLI half and the TUI half) and
//! both needed this surface; a second set of declarations inside one `impl`
//! block is a hand-untangled merge, while a second set across a file boundary
//! is a dedup. If the CLI half lands its own, delete whichever list is the
//! duplicate and keep ONE.
//!
//! Every call here is typed. `call` stays private on purpose: a public generic
//! call turns a typed client into a hole through which any caller can invoke
//! any method with any shape, and the typed surface is what keeps the TUI and
//! the macOS client honest about the same contract.

use ainb_hangar_proto::fleet::{
    FleetActivityListParams, FleetActivityListResult, FleetAdapterListResult,
    FleetChannelCreateParams, FleetChannelCreateResult, FleetChannelListResult,
    FleetConfirmAnswerParams, FleetConfirmAnswerResult, FleetConfirmListParams,
    FleetConfirmListResult, FleetCopilotConfigureParams, FleetCopilotConfigureResult,
};
use ainb_hangar_proto::methods;
use serde_json::json;

use crate::{DaemonClient, DaemonError};

impl DaemonClient {
    /// List the chat channels, in creation order.
    pub async fn channel_list(&self) -> Result<FleetChannelListResult, DaemonError> {
        let result = self.call(methods::FLEET_CHANNEL_LIST, json!({})).await?;
        serde_json::from_value(result).map_err(|error| DaemonError::Decode(error.to_string()))
    }

    /// The ACP adapters this daemon's registry can spawn.
    ///
    /// The engine picker's ONLY source. A list compiled into the client would
    /// refuse an adapter an operator has already configured, which is the
    /// closed-enum failure this call exists to remove.
    pub async fn adapter_list(&self) -> Result<FleetAdapterListResult, DaemonError> {
        let result = self.call(methods::FLEET_ADAPTER_LIST, json!({})).await?;
        serde_json::from_value(result).map_err(|error| DaemonError::Decode(error.to_string()))
    }

    /// Set the copilot's adapter, guardrail dial, model, reasoning and persona.
    ///
    /// A changed `provider` RETIRES the running session and mints a new one on
    /// the same channel, so the result's `session_key` may differ from the one
    /// the caller was holding; `session_replaced` says when it did.
    pub async fn copilot_configure(
        &self,
        params: FleetCopilotConfigureParams,
    ) -> Result<FleetCopilotConfigureResult, DaemonError> {
        let value = serde_json::to_value(params).expect("FleetCopilotConfigureParams serializes");
        let result = self.call(methods::FLEET_COPILOT_CONFIGURE, value).await?;
        serde_json::from_value(result).map_err(|error| DaemonError::Decode(error.to_string()))
    }

    /// Mint a chat channel and its `channel:<id>` scope.
    pub async fn channel_create(
        &self,
        params: FleetChannelCreateParams,
    ) -> Result<FleetChannelCreateResult, DaemonError> {
        let value = serde_json::to_value(params).expect("FleetChannelCreateParams serializes");
        let result = self.call(methods::FLEET_CHANNEL_CREATE, value).await?;
        serde_json::from_value(result).map_err(|error| DaemonError::Decode(error.to_string()))
    }

    /// List the OPEN guardrail confirm cards, oldest first.
    ///
    /// Returns the raw value rather than the typed result on purpose: a client
    /// older than the daemon must be able to SHOW a card whose state token it
    /// cannot decode, and a typed `Vec<FleetConfirm>` fails the whole frame on
    /// the first unknown one. The TUI decodes card by card and renders an
    /// undecodable card as unanswerable; see
    /// `ainb_plugin_hangar::screen::fleet_chat::ChatConfirmCard::decode`.
    pub async fn confirm_list_raw(
        &self,
        params: FleetConfirmListParams,
    ) -> Result<Vec<serde_json::Value>, DaemonError> {
        let value = serde_json::to_value(params).expect("FleetConfirmListParams serializes");
        let result = self.call(methods::FLEET_CONFIRM_LIST, value).await?;
        let confirms = result
            .get("confirms")
            .and_then(|confirms| confirms.as_array())
            .ok_or_else(|| DaemonError::Decode("confirm_list has no confirms array".into()))?;
        Ok(confirms.clone())
    }

    /// List the confirm cards, fully typed.
    ///
    /// The strict sibling of [`Self::confirm_list_raw`], for callers that would
    /// rather fail than render a card they do not understand.
    pub async fn confirm_list(
        &self,
        params: FleetConfirmListParams,
    ) -> Result<FleetConfirmListResult, DaemonError> {
        let value = serde_json::to_value(params).expect("FleetConfirmListParams serializes");
        let result = self.call(methods::FLEET_CONFIRM_LIST, value).await?;
        serde_json::from_value(result).map_err(|error| DaemonError::Decode(error.to_string()))
    }

    /// Answer one guardrail confirm card. Single-use: the daemon rejects a
    /// second answer to the same card rather than executing twice.
    pub async fn confirm_answer(
        &self,
        params: FleetConfirmAnswerParams,
    ) -> Result<FleetConfirmAnswerResult, DaemonError> {
        let value = serde_json::to_value(params).expect("FleetConfirmAnswerParams serializes");
        let result = self.call(methods::FLEET_CONFIRM_ANSWER, value).await?;
        serde_json::from_value(result).map_err(|error| DaemonError::Decode(error.to_string()))
    }

    /// Page the copilot activity feed by its commit-ordered `seq`.
    pub async fn activity_list(
        &self,
        params: FleetActivityListParams,
    ) -> Result<FleetActivityListResult, DaemonError> {
        let value = serde_json::to_value(params).expect("FleetActivityListParams serializes");
        let result = self.call(methods::FLEET_ACTIVITY_LIST, value).await?;
        serde_json::from_value(result).map_err(|error| DaemonError::Decode(error.to_string()))
    }
}
