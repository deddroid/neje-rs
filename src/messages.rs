//! Texts shared by the command line and the window, in the current language.

use crate::i18n::tr;
use crate::protocol::Event;

/// One line describing a message from the engraver.
pub fn describe(event: &Event) -> String {
    match *event {
        Event::Connected { hw: Some(hw) } => format!("{} (hardware {hw})", tr("Connected", "Connesso")),
        Event::Connected { hw: None } => tr("Connected", "Connesso").to_owned(),
        Event::Progress { x, y } => format!("X:{x} Y:{y}"),
        Event::Finished => tr("Engraving finished", "Incisione terminata").to_owned(),
        Event::NoPower => {
            tr("No power: plug in the 5 V power supply", "Alimentazione assente: collega l'alimentatore 5 V").to_owned()
        }
        Event::PowerOk => tr("Power supply present", "Alimentazione presente").to_owned(),
        Event::EraseDone => tr("Memory erased", "Memoria cancellata").to_owned(),
        Event::TransferImage => tr("Ready to receive the image", "Pronto a ricevere l'immagine").to_owned(),
        Event::Version(v) => format!("{}: {v}", tr("Hardware version", "Versione hardware")),
        Event::LowBatterySleep => tr("Sleeping: battery low", "In sospensione: batteria scarica").to_owned(),
        Event::TimeoutSleep => tr("Sleeping: idle for too long", "In sospensione: inattivo da troppo tempo").to_owned(),
        Event::Debug(v) => format!("Debug: {v}"),
        Event::Battery(v) => format!("{}: {v}", tr("Battery", "Batteria")),
        Event::BurnValue(v) => format!("{}: {v}", tr("Burn time on the machine", "Tempo di bruciatura sulla macchina")),
        Event::VerifyOk => tr("Verification OK", "Verifica OK").to_owned(),
        Event::Unknown(b) => format!("{}: {b:#04x}", tr("Unknown byte", "Byte sconosciuto")),
        Event::Disconnected => tr("Connection lost", "Connessione persa").to_owned(),
    }
}

/// What the operator must check before a command that switches the laser on.
pub fn laser_checklist() -> [&'static str; 3] {
    [
        tr("wear the protective goggles", "indossa gli occhiali protettivi"),
        tr("put the material under the head", "metti il materiale sotto la testa"),
        tr("keep anything flammable away", "allontana tutto ciò che è infiammabile"),
    ]
}
