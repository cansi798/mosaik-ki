//! Stufenwahl und Installation in der KI-Seitenleiste (Features `egui` + `install`).
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::Duration;

use crate::install::{self, InstallError, Progress, SystemInfo, fits, format_gb, recommended_tier};
use crate::{KiConfig, Tier, default_config_path};

pub(crate) enum Msg {
    Progress(Progress),
    Done(Result<KiConfig, InstallError>),
}

struct Job {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    progress: Option<Progress>,
    tier: Tier,
}

/// Was nach einem Bild mit der Auswahl passieren soll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    /// Auswahl bleibt offen.
    Open,
    /// „Zurück“ gedrückt.
    Close,
    /// Installation bzw. Entfernen ist fertig.
    Finished,
}

pub(crate) struct SetupView {
    info: SystemInfo,
    install_dir: Option<PathBuf>,
    /// Bereits installierte und nutzbare Stufe.
    current: Option<Tier>,
    selected: Tier,
    /// Mit „Zurück“ (KI ist schon nutzbar).
    closable: bool,
    job: Option<Job>,
    error: Option<(String, Tier)>,
}

impl SetupView {
    /// Liest Einstellung und Computer (schnell, ohne Netz).
    pub(crate) fn open(closable: bool) -> Self {
        let cfg = default_config_path().and_then(|p| KiConfig::load(&p));
        let current = cfg.as_ref().filter(|c| c.is_ready()).map(|c| c.tier);
        let install_dir = install::current_install_dir();
        let info = install_dir
            .as_deref()
            .map(install::system_check)
            .unwrap_or_else(|| install::system_check(&std::env::temp_dir()));
        Self::with_info(info, install_dir, current, closable)
    }

    pub(crate) fn with_info(
        info: SystemInfo,
        install_dir: Option<PathBuf>,
        current: Option<Tier>,
        closable: bool,
    ) -> Self {
        let selected = current.unwrap_or_else(|| recommended_tier(&info));
        Self {
            info,
            install_dir,
            current,
            selected,
            closable,
            job: None,
            error: None,
        }
    }

    pub(crate) fn is_busy(&self) -> bool {
        self.job.is_some()
    }

    fn start(&mut self, tier: Tier) {
        let Some(dir) = self.install_dir.clone() else {
            self.error = Some((InstallError::NoFolder.user_message(), tier));
            return;
        };
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        std::thread::spawn(move || {
            let result = install::install(
                tier,
                &dir,
                &mut |p| {
                    let _ = tx.send(Msg::Progress(p.clone()));
                },
                &flag,
            );
            let _ = tx.send(Msg::Done(result));
        });
        self.attach(rx, cancel, tier);
    }

    pub(crate) fn attach(&mut self, rx: Receiver<Msg>, cancel: Arc<AtomicBool>, tier: Tier) {
        self.error = None;
        self.job = Some(Job {
            rx,
            cancel,
            progress: None,
            tier,
        });
    }

    /// Holt Nachrichten des Hintergrund-Threads ab.
    pub(crate) fn poll(&mut self) -> Outcome {
        let Some(job) = &mut self.job else {
            return Outcome::Open;
        };
        loop {
            match job.rx.try_recv() {
                Ok(Msg::Progress(p)) => job.progress = Some(p),
                Ok(Msg::Done(Ok(_))) => {
                    self.job = None;
                    return Outcome::Finished;
                }
                Ok(Msg::Done(Err(e))) => {
                    self.error = Some((e.user_message(), job.tier));
                    self.job = None;
                    return Outcome::Open;
                }
                Err(TryRecvError::Empty) => return Outcome::Open,
                Err(TryRecvError::Disconnected) => {
                    self.error = Some((InstallError::Disk(String::new()).user_message(), job.tier));
                    self.job = None;
                    return Outcome::Open;
                }
            }
        }
    }

    pub(crate) fn show(&mut self, ui: &mut egui::Ui) -> Outcome {
        let outcome = self.poll();
        if outcome != Outcome::Open {
            return outcome;
        }
        let busy = self.is_busy();
        if self.closable {
            ui.label("KI-Stufe ändern");
        } else {
            ui.label("Die KI ist noch nicht installiert. Wählen Sie, welche KI Sie möchten:");
        }
        ui.add_space(4.0);
        let best = recommended_tier(&self.info);
        let mut tiers = vec![Tier::Kompakt, Tier::Pro];
        if self.current.is_some() {
            tiers.push(Tier::Standard);
        }
        for tier in tiers {
            self.card(ui, tier, best, busy);
            ui.add_space(4.0);
        }

        let mut click: Option<Click> = None;
        if let Some(job) = &self.job {
            let cancelling = job.cancel.load(Ordering::Relaxed);
            let (fraction, text) = match &job.progress {
                Some(p) => (p.fraction(), p.text.clone()),
                None => (0.0, "Installation wird vorbereitet \u{2026}".into()),
            };
            ui.add(egui::ProgressBar::new(fraction).show_percentage());
            ui.label(if cancelling {
                "Wird abgebrochen \u{2026}".to_string()
            } else {
                text
            });
            if ui
                .add_enabled(!cancelling, egui::Button::new("Abbrechen"))
                .clicked()
            {
                click = Some(Click::Cancel);
            }
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        } else {
            if let Some((msg, tier)) = &self.error {
                ui.colored_label(ui.visuals().warn_fg_color, msg.as_str());
                if ui.button("Nochmal versuchen").clicked() {
                    click = Some(Click::Start(*tier));
                }
                ui.add_space(4.0);
            }
            let selected = self.selected;
            let allowed = fits(selected, &self.info).is_ok() && Some(selected) != self.current;
            let label = if selected == Tier::Standard {
                "KI entfernen"
            } else {
                "KI installieren"
            };
            ui.horizontal(|ui| {
                if ui.add_enabled(allowed, egui::Button::new(label)).clicked() {
                    click = Some(Click::Start(selected));
                }
                if self.closable && ui.button("Zurück").clicked() {
                    click = Some(Click::Back);
                }
            });
        }
        match click {
            Some(Click::Cancel) => {
                if let Some(job) = &self.job {
                    job.cancel.store(true, Ordering::Relaxed);
                }
                Outcome::Open
            }
            Some(Click::Start(tier)) => {
                self.start(tier);
                Outcome::Open
            }
            Some(Click::Back) => Outcome::Close,
            None => Outcome::Open,
        }
    }

    fn card(&mut self, ui: &mut egui::Ui, tier: Tier, best: Tier, busy: bool) {
        let fit = fits(tier, &self.info);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let title = if tier == Tier::Standard {
                "Ohne KI"
            } else {
                tier.label()
            };
            ui.add_enabled_ui(fit.is_ok() && !busy, |ui| {
                ui.radio_value(&mut self.selected, tier, title);
            });
            if Some(tier) == self.current {
                ui.label("Ist installiert");
            } else if tier == best && fit.is_ok() && tier.has_ki() {
                ui.label("\u{2713} Passt zu Ihrem Computer");
            }
            if tier == Tier::Standard {
                ui.label("Die KI wird entfernt und gibt Speicherplatz frei.");
                return;
            }
            for example in tier.examples() {
                ui.label(format!("\u{2022} {example}"));
            }
            ui.label(format!(
                "Arbeitsspeicher: {} GB \u{2013} Ihr Computer: {}",
                tier.ram_gb(),
                self.info.ram_text()
            ));
            let needed = self.info.needed(tier);
            if needed > 0 {
                let free = self.info.free_text().unwrap_or_else(|| "unbekannt".into());
                ui.label(format!(
                    "Speicherplatz: noch {} \u{2013} frei: {free}",
                    format_gb(needed)
                ));
            }
            if let Err(why) = &fit {
                ui.colored_label(ui.visuals().warn_fg_color, why.as_str());
            }
        });
    }
}

#[derive(Debug, Clone, Copy)]
enum Click {
    Cancel,
    Start(Tier),
    Back,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(ram_gib: u64) -> SystemInfo {
        SystemInfo {
            ram_bytes: ram_gib * 1024 * 1024 * 1024,
            free_bytes: Some(100_000_000_000),
            needed_bytes: [0, 4_000_000_000, 6_500_000_000],
            ki_supported: true,
        }
    }

    fn texts(view: &mut SetupView) -> (Outcome, String) {
        let ctx = egui::Context::default();
        let mut outcome = Outcome::Open;
        let out = ctx.run_ui(egui::RawInput::default(), |ui| outcome = view.show(ui));
        let mut texts = String::new();
        for clipped in &out.shapes {
            texts.push_str(&format!("{:?}", clipped.shape));
        }
        out.drop_without_applying_deltas();
        (outcome, texts)
    }

    #[test]
    fn best_fitting_tier_is_preselected_and_unfit_card_shows_reason() {
        let mut view = SetupView::with_info(info(8), None, None, false);
        assert_eq!(view.selected, Tier::Kompakt);
        let (outcome, _) = texts(&mut view);
        assert_eq!(outcome, Outcome::Open);
        assert_eq!(
            SetupView::with_info(info(16), None, None, false).selected,
            Tier::Pro
        );
        assert_eq!(
            SetupView::with_info(info(16), None, Some(Tier::Kompakt), true).selected,
            Tier::Kompakt
        );
    }

    #[test]
    fn progress_then_success_finishes() {
        let mut view = SetupView::with_info(info(16), None, None, false);
        let (tx, rx) = channel();
        view.attach(rx, Arc::new(AtomicBool::new(false)), Tier::Pro);
        tx.send(Msg::Progress(Progress {
            phase: install::Phase::Downloading,
            done: 1,
            total: 2,
            text: "KI wird heruntergeladen \u{2026} 0,0 von 0,0 GB".into(),
        }))
        .unwrap();
        assert_eq!(view.poll(), Outcome::Open);
        assert!(view.is_busy());
        let _ = texts(&mut view);
        tx.send(Msg::Done(Ok(KiConfig::new(Tier::Pro, PathBuf::from("/x")))))
            .unwrap();
        assert_eq!(view.poll(), Outcome::Finished);
        assert!(!view.is_busy());
    }

    #[test]
    fn failure_offers_retry_with_same_tier() {
        let mut view = SetupView::with_info(info(16), None, None, false);
        let (tx, rx) = channel();
        view.attach(rx, Arc::new(AtomicBool::new(false)), Tier::Kompakt);
        tx.send(Msg::Done(Err(InstallError::Offline("x".into()))))
            .unwrap();
        assert_eq!(view.poll(), Outcome::Open);
        assert_eq!(
            view.error,
            Some((
                InstallError::Offline(String::new()).user_message(),
                Tier::Kompakt
            ))
        );
        let (_, shown) = texts(&mut view);
        assert!(shown.contains("Nochmal versuchen"), "{shown}");
    }

    #[test]
    fn missing_folder_becomes_message_instead_of_thread() {
        let mut view = SetupView::with_info(info(16), None, None, false);
        view.start(Tier::Kompakt);
        assert!(!view.is_busy());
        assert!(view.error.is_some());
    }
}
