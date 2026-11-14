# neje

Software libero per l'incisore laser **NEJE DK-8-KZ**, per Linux (e Windows).
Sostituisce il programma NEJE V3.6, abbandonato e solo per Windows: prepari un'immagine, un
testo o un disegno in G-code, lo invii all'incisore e avvii il lavoro, da una finestra o dal
terminale.

[Read it in English](README.md)

![La finestra con un disegno G-code caricato](docs/screenshots/it_light.png)

## Stato

| Parte | Stato |
|---|---|
| Preparazione dell'immagine (riduzione, grigio, Otsu, retinatura NEJE, BMP a 1 bit) | stessi byte dell'implementazione di riferimento su oltre 21 000 casi |
| Comandi e messaggi dei protocolli V10, V20, V21, V30 | stessi byte dell'implementazione di riferimento |
| Rilevamento, cancellazione, invio, avvio, avanzamento, fine lavoro | provati con l'incisore virtuale incluso |
| Una DK-8-KZ vera | **non ancora comandata da questo programma**; stretta di mano e movimenti sono stati provati su una macchina con il prototipo Python da cui deriva (vedi [docs/PROTOCOL.md](docs/PROTOCOL.md)) |
| Windows | la compilazione dovrebbe funzionare, mai provata |

La DK-8-KZ ha un'area di lavoro di 36,75 × 36,75 mm: 490 × 490 pixel da 0,075 mm.

## Sicurezza laser

- Il diodo laser (405 nm) è acceso a bassa potenza **appena colleghi l'alimentatore 5 V**:
  indossa gli occhiali protettivi ogni volta che la macchina è alimentata.
- `start`, `engrave`, `box`, `forward`, `backward` e `redo` accendono il laser. Il programma
  chiede prima una conferma: occhiali indossati, materiale sotto la testa, niente di
  infiammabile vicino. Non lasciare mai un lavoro in corso senza sorveglianza.
- `start` incide l'immagine conservata nella macchina, che resta anche da spenta: su una
  macchina appena collegata può essere una vecchia immagine.
- Questo programma non scrive mai nel firmware dell'incisore.

## Installazione

Serve una toolchain Rust (1.88 o successiva). Su Arch/CachyOS non è servito altro per
compilare; la finestra usa OpenGL e le finestre di scelta file del desktop.

```sh
git clone <questo repository>
cd neje-rs
./install.sh            # compila e installa in ~/.local (senza root)
./install.sh --udev     # installa anche la regola udev (chiede la password di sudo)
./install.sh --uninstall
```

Il convertitore USB-seriale dell'incisore (CH340, `1a86:7523`) è gestito dal kernel Linux:
compare come `/dev/ttyUSB0`. Per aprirlo serve la regola udev
([packaging/linux/70-neje.rules](packaging/linux/70-neje.rules), accesso all'utente che ha
la sessione aperta sul computer) oppure l'appartenenza al gruppo proprietario della porta
(`uucp` su Arch, `dialout` su Debian e Ubuntu).

Senza interfaccia grafica: `cargo build --release --no-default-features`.

## Uso

### Finestra

```sh
neje-gui                 # oppure il lanciatore "Incisore NEJE"
neje-gui foto.jpg        # apre subito un'immagine o un file G-code
```

Apri un'immagine (o trascinala sulla finestra), scegli la conversione, connetti, invia
l'immagine e poi avvia. La traccia rossa sull'anteprima segue le posizioni comunicate
dall'incisore. La lingua si cambia in fondo al pannello, nel gruppo "Applicazione".

### Terminale

```sh
neje ports                               # elenca le porte seriali
neje detect                              # trova l'incisore
neje preview foto.jpg -o anteprima.png   # cosa verrebbe inciso, senza macchina
neje preview foto.jpg --mode dither      # retinatura NEJE, per le foto
neje preview --gcode examples/rosette.nc --fit --text "NEJE" --size 40
neje upload foto.jpg                     # cancella la memoria e invia l'immagine
neje engrave foto.jpg --burn 30          # invia e poi incide (chiede conferma)
neje start --burn 30                     # incide l'immagine già nella macchina
neje pause                               # anche Ctrl+C durante "engrave" mette in pausa
neje center | home | move left 5 | box | set-burn 40 | monitor
```

`neje --help` e `neje <comando> --help` elencano tutte le opzioni. Le più utili:

- `--port /dev/ttyUSB0`: salta la ricerca (e fa passare `pause`/`reset` anche quando il
  firmware è troppo occupato per rispondere);
- `--protocol v10|v20|v21|v30|auto`: il predefinito è `v10`, il protocollo della DK-8-KZ.
  `auto` li prova tutti come il lanciatore originale; evitalo su una macchina V10;
- `--lang en|it`: lingua dei messaggi (predefinita: quella del sistema).

### Provarlo senza la macchina

```sh
neje simulate                    # stampa il percorso di una porta seriale virtuale
neje --port /dev/pts/N engrave foto.jpg --yes
```

L'incisore virtuale risponde come il firmware V10 (o, con `--sim-protocol v30`, il V30),
salva l'immagine che riceve e segnala gli errori di protocollo, per esempio i dati inviati
durante la cancellazione.

## Come è stato verificato

- `cargo test`: test unitari della libreria e test completi della CLI e della sessione
  contro l'incisore virtuale.
- `tests/parity`: il prototipo Python (a sua volta confrontato bit per bit con il codice del
  programma originale) scrive lo SHA-256 del proprio risultato per migliaia di ingressi, e il
  test Rust li ricalcola. Vedi l'intestazione di [tests/parity.rs](tests/parity.rs).
- `NEJE_DEBUG=1` stampa il traffico seriale grezzo; `NEJE_GUI_SCREENSHOTS=<cartella>` salva
  uno screenshot della finestra per ogni tema e lingua.

## Limiti noti

- File JPEG: i decodificatori differiscono di un livello su pochi pixel, quindi il risultato
  può differire di qualche punto dal prototipo Python e dal programma originale (che usava
  il decodificatore di Windows). Tutto ciò che viene dopo la decodifica è identico. PNG, BMP
  e gli altri formati sono esatti.
- Il testo è disegnato con un motore di font diverso da quello del prototipo: stessa
  disposizione, non gli stessi pixel.
- I PNG con un colore trasparente in modalità RGB e i PNG a 16 bit sono letti in modo
  leggermente diverso dal prototipo.
- Movimenti della testa (`move`): la macchina non ha finecorsa e un passo non è un pixel;
  muovi pochi passi alla volta.

## Crediti

- Daniele ([deddroid](https://github.com/deddroid)), proprietario dell'incisore, che ha
  fatto le prove sulla macchina.
- Claude (Anthropic), che ha scritto il codice con lui.
- Il protocollo viene dal programma ufficiale NEJE V3.6, studiato per l'interoperabilità, ed
  è stato confrontato con [EzGraver](https://github.com/camrein/EzGraver) e
  [nejemojo](https://github.com/sthysel/nejemojo).
- Font: [Inter](https://rsms.me/inter/) e
  [Rajdhani](https://github.com/itfoundry/rajdhani), SIL Open Font License
  (vedi [assets/fonts](assets/fonts)).

NEJE è un marchio del rispettivo proprietario; questo progetto non è affiliato.

## Licenza

Non ancora scelta: verrà definita prima della prima pubblicazione.
