use anyhow::Result;
use notify::{RecursiveMode, Watcher};
use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::mpsc;

pub fn run(file_path: &str) -> Result<()> {

    // Créer un tracker d'offsets
    let offset_tracker2 = &mut OffsetTracker::new();
    offset_tracker2.add_offset(file_path)?;

    // Afficher le contenu initial du fichier
    // read_and_display_file(file_path, &offset_tracker)?;

    // Créer un channel pour recevoir les événements
    let (tx, rx) = mpsc::channel();

    // Créer un watcher
    let mut watcher = notify::recommended_watcher(move |res| {
        tx.send(res).unwrap();
    })?;

    // Commencer à surveiller le fichier
    watcher.watch(Path::new(file_path), RecursiveMode::NonRecursive)?;

    println!(
        "Surveillance des changements dans {} (Ctrl+C pour arrêter)",
        file_path
    );

    for result in rx {
        match result {
            Ok(event) => {
                println!("{:?}", event);
                if event.kind.is_modify() {
                    // Lire à partir de l'offset enregistré
                    // read_from_offset(file_path, &offset_tracker)?;
                    // read_line_by_line(file_path, &offset_tracker)?;
                    read_line_by_line(file_path, offset_tracker2)?;
                }
            }
            Err(e) => {
                println!("Error: {}", e);
            }
        }
    }

    Ok(())
}

struct FileBuffer {
    // current_line: u64,
    // offset: u64,
    reader: BufReader<File>,
}

// Structure pour gérer les offsets de lecture
struct OffsetTracker {
    offsets: HashMap<String, FileBuffer>,
}

impl OffsetTracker {
    fn new() -> Self {
        Self {
            offsets: HashMap::new(),
        }
    }

    fn add_offset(&mut self, file_path: &str) -> Result<()> {
        let file = File::open(file_path)?;
        let reader = BufReader::new(file);

        let tracker = FileBuffer {
            // current_line: 0,
            // offset: 0,
            reader,
        };

        // let mut offsets = self.offsets.lock().unwrap();
        self.offsets.insert(file_path.to_string(), tracker);
        Ok(())
    }

    fn get_offset(&mut self, file_path: &str) -> Result<&mut FileBuffer> {
        // let offsets = self.offsets.lock().unwrap();
        let x = self.offsets.get_mut(file_path).unwrap();
        Ok(x)
    }
}

fn read_line_by_line(file_path: &str, offset_tracker: &mut OffsetTracker) -> Result<()> {
    // print!("-");

    let fb = offset_tracker.get_offset(file_path)?;
    let mut buf = String::new();

    loop {
        let num_bytes = fb.reader.read_line(&mut buf)?;
        if num_bytes == 0 {
            break;
        }
        // fb.current_line += 1;
        print!("{}", buf);
        buf.clear();
        io::stdout().flush()?;
    }

    Ok(())
}
