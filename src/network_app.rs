//! The Network source in the application (0.4.0, spec 21.2): the server's
//! address and token, asking it for its inputs, and probing one.

use crate::app::{AppState, PromptTarget};
use crate::source::model::{SourceField, SourceKind, SourceModule};
use crate::ui::prompt::TextPrompt;

impl AppState {
    /// After the source kind changed: a Network source not yet asked asks
    /// its server.
    pub fn entered_source_kind(&mut self) {
        let s = &self.source;
        if s.kind == SourceKind::Network
            && s.devices.is_empty()
            && !s.network.addr.trim().is_empty()
        {
            self.network_query();
        }
    }

    /// ENTER on the Server or Token row: edit it.
    pub fn activate_network_field(&mut self) {
        let (title, value, target) = match self.source.field() {
            SourceField::NetServer => (
                "Server (host or host:port)",
                self.source.network.addr.clone(),
                PromptTarget::NetServer,
            ),
            SourceField::NetToken => (
                "Server token (empty: none)",
                self.source.network.token.clone(),
                PromptTarget::NetToken,
            ),
            _ => return,
        };
        self.prompt = Some(TextPrompt::new(title, &value));
        self.prompt_target = Some(target);
    }

    /// A new server address: ask it what it offers.
    pub fn set_network_server(&mut self, addr: &str) {
        self.source.network.addr = addr.to_string();
        self.network_query();
    }

    /// Ask the server for its inputs and the selected one's capabilities
    /// (blocking, a few seconds at most); the Server row's status line says
    /// what came of it.
    pub fn network_query(&mut self) {
        let selected = self.source.selected_device_args();
        self.network_query_for(selected);
    }

    /// [`Self::network_query`] keeping `selected` selected when the server
    /// still offers it (the saved input at startup).
    pub fn network_query_for(&mut self, selected: Option<String>) {
        if self.source.kind != SourceKind::Network {
            return;
        }
        let n = &self.source.network;
        if n.addr.trim().is_empty() {
            self.source.network.status = None;
            return;
        }
        let _hush = crate::engine::silence::StdioSilencer::new();
        match crate::remote::client::query(n.addr.trim(), n.token().as_deref(), selected.as_deref())
        {
            Ok(q) if q.info.window => {
                // a sharing ShellSDR: its screen, no devices
                self.source.set_network_inputs(Vec::new(), None, None);
                self.source.network.window = true;
                self.source.network.status = Some(Ok(format!(
                    "{} v{}: shared screen",
                    q.info.host, q.info.version
                )));
            }
            Ok(q) => {
                self.source.network.window = false;
                let caps = q.caps.as_ref().and_then(|c| c.as_ref().ok());
                let chosen = selected.or_else(|| q.inputs.first().map(|i| i.args.clone()));
                self.source
                    .set_network_inputs(q.inputs.clone(), chosen.as_deref(), caps);
                let audio = if q.info.opus { "Opus" } else { "PCM" };
                self.source.network.status = Some(match q.caps {
                    Some(Err(e)) => Err(e),
                    _ if q.inputs.is_empty() => Err(format!("{}: no inputs", q.info.host)),
                    _ => Ok(format!("{} v{} {audio}", q.info.host, q.info.version)),
                });
                self.apply_network_settings();
                self.apply_file_center();
            }
            Err(e) => self.source.network.status = Some(Err(e)),
        }
    }

    /// Re-apply the saved settings of the selected server input.
    fn apply_network_settings(&mut self) {
        if let Some(args) = self.source.network_device_args() {
            let key = SourceModule::network_key(&self.source.network.addr, &args);
            if let Some(s) = self.source_devices.get(&key).cloned() {
                self.source.apply_device_settings(&s);
            }
        }
    }

    /// Select the source device at `idx` and probe it: a local device, or
    /// one of the server's inputs.
    pub fn select_source_device(&mut self, idx: usize) {
        if self.source.kind != SourceKind::Network {
            self.source.select_device(idx, Self::probe_device_caps);
            return;
        }
        let addr = self.source.network.addr.trim().to_string();
        let token = self.source.network.token();
        let mut err = None;
        let _hush = crate::engine::silence::StdioSilencer::new();
        let ok = self.source.select_device(idx, |args| {
            crate::remote::client::probe(&addr, token.as_deref(), args)
                .map_err(|e| err = Some(e))
                .ok()
        });
        if ok {
            self.apply_network_settings();
            self.apply_file_center();
        } else if let Some(e) = err {
            self.source.network.status = Some(Err(e));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::RunState;

    #[test]
    fn start_on_a_sharing_server_opens_its_window() {
        let mut s = AppState::with_mock_source();
        s.source.kind = SourceKind::Network;
        s.source.network.window = true;
        s.toggle_run();
        assert_eq!(s.run, RunState::Stopped, "no stream to start");
        assert!(s.source.network.open_request);
        s.source.network.window = false;
        s.toggle_run();
        assert_eq!(s.run, RunState::Running, "a server's input starts");
    }
}
