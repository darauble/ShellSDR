//! Sharing this ShellSDR in the application (0.4.1, spec 22.2): the Source
//! module's sharing rows. The run loop owns the server itself
//! (`remote::share::ShareServer`) and follows these settings.

use crate::app::{AppState, PromptTarget};
use crate::source::model::SourceField;
use crate::ui::prompt::TextPrompt;

impl AppState {
    /// ENTER or a click on a sharing row.
    pub fn activate_share_field(&mut self) {
        let field = self.source.field();
        let sh = &mut self.source.share;
        match field {
            SourceField::ShareOn => {
                sh.on = !sh.on;
                if !sh.on {
                    sh.status = None;
                }
            }
            SourceField::ShareMute => sh.mute_while_viewed = !sh.mute_while_viewed,
            SourceField::ShareDisconnect => sh.disconnect = true,
            SourceField::ShareListen => {
                let v = sh.listen.clone();
                self.prompt = Some(TextPrompt::new(
                    "Share on (host:port; 0.0.0.0 = every interface)",
                    &v,
                ));
                self.prompt_target = Some(PromptTarget::ShareListen);
            }
            SourceField::ShareToken => {
                let v = sh.token.clone();
                self.prompt = Some(TextPrompt::new("Share token (empty: none)", &v));
                self.prompt_target = Some(PromptTarget::ShareToken);
            }
            _ => {}
        }
    }

    /// Left/Right on a sharing row: the switches flip.
    pub fn share_adjust(&mut self) {
        if matches!(
            self.source.field(),
            SourceField::ShareOn | SourceField::ShareMute
        ) {
            self.activate_share_field();
        }
    }

    /// Whether the audio here is muted because a viewer listens.
    pub fn muted_for_viewer(&self) -> bool {
        let sh = &self.source.share;
        sh.mute_while_viewed && sh.viewer.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sharing_rows_follow_every_kind() {
        let mut s = AppState::with_mock_source();
        let k = s.source.kind_rows();
        assert_eq!(s.source.field_row(SourceField::ShareOn), k);
        assert!(s.source.fields().contains(&SourceField::ShareToken));
        assert!(!s.source.fields().contains(&SourceField::ShareDisconnect));
        s.source.select_field(SourceField::ShareOn);
        s.activate_share_field();
        assert!(s.source.share.on);
        s.source.share.viewer = Some("10.0.0.2 (terminal, 80x24)".into());
        assert!(s.source.fields().contains(&SourceField::ShareDisconnect));
        assert_eq!(s.source.content_rows(), k + 6);
        s.source.select_field(SourceField::ShareMute);
        s.activate_share_field();
        assert!(s.muted_for_viewer());
        assert_eq!(s.effective_volume(), 0.0, "muted here while viewed");
        s.toggle_source_kind();
        assert_eq!(
            s.source.field_row(SourceField::ShareOn),
            s.source.kind_rows(),
            "the File view too"
        );
    }
}
