//! Texts shared by the command line and the window, in the current language.

use crate::doctor::Problem;
use crate::i18n::tr;
use crate::plan::PrintMode;
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
        Event::PowerOk => tr(
            "Initialisation completed (the engraver has restarted)",
            "Inizializzazione riuscita (l'incisore si è riavviato)",
        )
        .to_owned(),
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

/// Short name of a print mode.
pub fn mode_name(mode: PrintMode) -> String {
    match mode {
        PrintMode::Single => tr("Single job", "Lavoro unico").to_owned(),
        PrintMode::Split => tr("Split in jobs", "Diviso in lavori").to_owned(),
        PrintMode::Line { stroke } => format!("{} ({stroke} px)", tr("Line art", "A tratto")),
    }
}

/// What a print mode does and when to pick it.
pub fn mode_description(mode: PrintMode) -> &'static str {
    match mode {
        PrintMode::Single => tr(
            "The image is sent and engraved as it is, in one go. On every row the head runs from the first to the last black pixel and crosses the empty space in between.",
            "L'immagine viene inviata e incisa così com'è, in una volta sola. In ogni riga la testa va dal primo all'ultimo pixel nero e attraversa lo spazio vuoto che c'è in mezzo.",
        ),
        PrintMode::Split => tr(
            "The image is cut in vertical bands, engraved one after the other, when the empty space no longer crossed is worth more than one more upload (about 12 s). The bands line up on the piece: do not restart or switch off the engraver between them.",
            "L'immagine viene tagliata in fasce verticali, incise una dopo l'altra, quando lo spazio vuoto non più attraversato vale più di un invio in più (circa 12 s). Le fasce combaciano sul pezzo: non riavviare né spegnere l'incisore fra una e l'altra.",
        ),
        PrintMode::Line { .. } => tr(
            "Shapes are reduced to their centre line and drawn with a stroke of a few pixels instead of being filled: one pass per pixel of width. Much faster for drawings, text and logos; not meant for photos. Split in several jobs when that is faster.",
            "Le forme vengono ridotte alla loro linea centrale e disegnate con un tratto di pochi pixel invece di essere riempite: una passata per ogni pixel di spessore. Molto più veloce per disegni, scritte e loghi; non adatta alle foto. Divisa in più lavori quando conviene.",
        ),
    }
}

/// One line saying what the system lacks.
pub fn problem_title(problem: &Problem) -> String {
    match problem {
        Problem::DriverNotBound { .. } => tr(
            "The engraver is plugged in but has no serial port",
            "L'incisore è collegato ma non ha una porta seriale",
        )
        .to_owned(),
        Problem::NoAccess { port } => {
            format!("{} {port}", tr("You are not allowed to open", "Non hai il permesso di aprire"))
        }
    }
}

/// Why it happens and what solves it.
pub fn problem_text(problem: &Problem) -> &'static str {
    match problem {
        Problem::DriverNotBound { brltty: true } => tr(
            "The braille service \"brltty\" is installed and takes the engraver's USB converter (CH340) for a braille display. Remove brltty, or disable its udev rule, then plug the engraver in again.",
            "È installato il servizio braille \"brltty\", che scambia il convertitore USB dell'incisore (CH340) per un display braille. Rimuovi brltty, o disattiva la sua regola udev, poi ricollega l'incisore.",
        ),
        Problem::DriverNotBound { brltty: false } => tr(
            "The kernel driver of its USB converter (ch341) is not loaded. Loading it needs the administrator password.",
            "Il driver del kernel per il suo convertitore USB (ch341) non è caricato. Per caricarlo serve la password di amministratore.",
        ),
        Problem::NoAccess { .. } => tr(
            "The port belongs to the administrator. A udev rule can give access to whoever is logged in at this computer; installing it needs the administrator password.",
            "La porta appartiene all'amministratore. Una regola udev può dare l'accesso a chi ha la sessione aperta su questo computer; per installarla serve la password di amministratore.",
        ),
    }
}

/// Text of the button that runs the fix of a problem.
pub fn problem_fix_label(problem: &Problem) -> &'static str {
    match problem {
        Problem::DriverNotBound { .. } => tr("Load the driver", "Carica il driver"),
        Problem::NoAccess { .. } => tr("Install the access rule", "Installa la regola di accesso"),
    }
}
