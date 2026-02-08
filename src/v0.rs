use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;
use std::sync::{mpsc, Arc, Mutex};
use notify::{RecursiveMode, Watcher};
use anyhow::Result;

#[derive(Clone, Copy)]
struct FileOffset {
    current_line: u64,
    offset: u64,
}

impl FileOffset {
    fn default() -> FileOffset {
        FileOffset{current_line: 0, offset: 0}
    }
}

// Structure pour gérer les offsets de lecture
struct OffsetTracker {
    offsets: Arc<Mutex<HashMap<String, FileOffset>>>,
}

impl OffsetTracker {
    fn new() -> Self {
        Self {
            offsets: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn get_offset(&self, file_path: &str) -> anyhow::Result<FileOffset> {
        let offsets = self.offsets.lock().unwrap();
        Ok(
            *offsets
                .get(file_path)
                .unwrap_or(&FileOffset::default())
        )
    }

    fn update_offset(&self, file_path: &str, line: u64) -> anyhow::Result<()> {
        // Obtenir la taille actuelle du fichier
        let metadata = std::fs::metadata(file_path)?;
        let file_size = metadata.len();

        let fo = FileOffset{
            current_line: line,
            offset: file_size,
        };

        let mut offsets = self.offsets.lock().unwrap();
        offsets.insert(file_path.to_string(), fo);

        Ok(())
    }
}

pub fn run() -> Result<()> {
    let file_path = "_geek/small.txt";

    // Créer un tracker d'offsets
    let offset_tracker = OffsetTracker::new();

    // Afficher le contenu initial du fichier
    read_and_display_file(file_path, &offset_tracker)?;

    // Créer un channel pour recevoir les événements
    let (tx, rx) = mpsc::channel();

    // Créer un watcher
    let mut watcher = notify::recommended_watcher(move |res| {
        tx.send(res).unwrap();
    })?;

    // Commencer à surveiller le fichier
    watcher.watch(Path::new(file_path), RecursiveMode::NonRecursive)?;

    println!("Surveillance des changements dans {} (Ctrl+C pour arrêter)", file_path);

    for result in rx {
        match result {
            Ok(event) => {
                // println!("{:?}", event);
                if event.kind.is_modify() {
                    // Lire à partir de l'offset enregistré
                    // read_from_offset(file_path, &offset_tracker)?;
                    read_line_by_line(file_path, &offset_tracker)?;
                }
            }
            Err(e) => {
                println!("Error: {}", e);
            }
        }
    }

    Ok(())
}

fn read_and_display_file(file_path: &str, offset_tracker: &OffsetTracker) -> anyhow::Result<()> {
    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    let mut line_count: u64 = 0;

    println!("Contenu du fichier {} :", file_path);
    for (index, line) in reader.lines().enumerate() {
        line_count += 1;
        println!("{:>3}: {}", index + 1, line?);
    }

    // Mettre à jour l'offset après avoir lu le fichier entier
    offset_tracker.update_offset(file_path, line_count - 1)?;

    Ok(())
}

fn read_line_by_line(file_path: &str, offset_tracker: &OffsetTracker) -> anyhow::Result<()> {
    let fo = offset_tracker.get_offset(file_path)?;
    let mut current_line: u64 = fo.current_line;

    let mut file = File::open(file_path)?;

    // Se positionner à l'offset
    file.seek(SeekFrom::Start(fo.offset))?;
    // let mut cursor = io::Cursor::new(b"foo\nbar");

    let mut buf = String::new();

    let mut reader = BufReader::new(file);
    loop {
        let num_bytes = reader.read_line(&mut buf)?;
        if num_bytes == 0 {
            break;
        }
        current_line += 1;
        print!("{}", buf);
        buf.clear();
    }

    // Mettre à jour l'offset
    offset_tracker.update_offset(file_path, current_line)?;

    Ok(())
}


fn read_from_offset(file_path: &str, offset_tracker: &OffsetTracker) -> anyhow::Result<()> {
    // Obtenir l'offset actuel
    let fo = offset_tracker.get_offset(file_path)?;

    // Ouvrir le fichier
    let mut file = File::open(file_path)?;

    // Se positionner à l'offset
    file.seek(SeekFrom::Start(fo.offset))?;

    // Créer un lecteur à partir de cette position
    let reader = BufReader::new(file);

    // println!("Nouveau contenu à partir de l'offset {} :", current_offset);

    let mut current_line: u64 = fo.current_line;
    for line in reader.lines() {
        current_line += 1;
        println!("{}", line?);
    }

    // Mettre à jour l'offset
    offset_tracker.update_offset(file_path, current_line)?;

    Ok(())
}
