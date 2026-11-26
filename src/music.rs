//! The Music app: every song in Solon storage, on USB drives and phones, and the demo songs
//! on the Solon stick (or the disk Solon was installed on). Songs by title, artist, album or
//! length; albums with their covers; artists. A player with previous/next, shuffle, repeat
//! and volume that keeps playing while other windows are in front (one song runs straight
//! into the next). Any song can be opened in the Sound Editor.
//!
//! Plays MP3, AAC/M4A, FLAC, Ogg Vorbis, Opus, WAV, AIFF, AMR, AC-3 and DTS (decoders in
//! `media`); tags and covers come from `media::tags`, or a folder.jpg next to the songs.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::Ordering::Relaxed;

use crate::fs::DirEntry;
use crate::gfx::{rgb, text_width, Canvas, Rect, Size};
use crate::image::Image;
use crate::interrupts::{HZ, TICKS};
use crate::library::Storage;
use crate::media::Engine;
use crate::movie::Io;
use crate::movieui::LibIo;
use crate::photos::{ellipsize, Source};
use crate::usb::Usb;

const BG: u32 = rgb(250, 250, 252);
const PANEL: u32 = rgb(241, 242, 246);
const LINE: u32 = rgb(228, 230, 236);
const INK: u32 = rgb(28, 32, 40);
const DIM: u32 = rgb(112, 120, 132);
const ACCENT: u32 = rgb(208, 46, 106);
const SEL: u32 = rgb(251, 227, 237);
const WHITE: u32 = rgb(255, 255, 255);

const TOP_H: i32 = 56;
const HEAD_H: i32 = 34;
const ROW_H: i32 = 38;
const BAR_H: i32 = 96;
/// Album tiles.
const TILE_W: i32 = 190;
const TILE_H: i32 = 246;
const COVER: usize = 170;

/// File names Solon tries as songs (what's inside decides whether they play).
pub fn is_music(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [".mp3", ".m4a", ".m4b", ".aac", ".flac", ".ogg", ".oga", ".opus", ".wav", ".aif", ".aiff", ".aifc", ".amr", ".mka", ".mp2", ".mpa", ".ac3", ".dts", ".wma"].iter().any(|e| n.ends_with(e))
}

/// A picture that serves as the cover of the songs beside it (Windows Media Player's and
/// iTunes' names), rather than a photo.
pub fn is_cover_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["folder.jpg", "folder.png", "cover.jpg", "cover.png", "front.jpg", "album.jpg"].contains(&n.as_str()) || (n.starts_with("albumart") && (n.ends_with(".jpg") || n.ends_with(".png")))
}

fn time_text(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

pub struct Song {
    pub name: String,
    /// Where it is: "Solon storage", a drive's or phone's name, "Demo songs".
    pub place: String,
    pub src: Source,
    pub size: u64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub year: String,
    pub track: u32,
    pub disc: u32,
    pub dur_ms: Option<u64>,
    /// Index into the covers.
    cover: Option<usize>,
    /// The folder it's in (for a folder.jpg), as an index into `folders`.
    folder: Option<usize>,
    tagged: bool,
    pub problem: Option<String>,
}

impl Song {
    fn new(name: String, place: String, src: Source, size: u64, folder: Option<usize>) -> Song {
        // Until the tags are read: "Artist - Title.mp3" or just the name.
        let stem = name.rsplit_once('.').map(|(a, _)| a).unwrap_or(&name);
        let stem = stem.trim_start_matches(|c: char| c.is_ascii_digit() || c == ' ' || c == '-' || c == '.' || c == '_');
        let (artist, title) = match stem.split_once(" - ") {
            Some((a, t)) => (String::from(a.trim()), String::from(t.trim())),
            None => (String::new(), String::from(if stem.is_empty() { &name } else { stem })),
        };
        Song { name, place, src, size, title, artist, album: String::new(), year: String::new(), track: 0, disc: 0, dur_ms: None, cover: None, folder, tagged: false, problem: None }
    }
    fn artist_or_unknown(&self) -> &str {
        if self.artist.is_empty() {
            "Unknown artist"
        } else {
            &self.artist
        }
    }
    fn album_or_unknown(&self) -> &str {
        if self.album.is_empty() {
            "Unknown album"
        } else {
            &self.album
        }
    }
}

/// A folder songs were found in, and a picture in it that serves as their cover.
struct FolderArt {
    art: Option<Source>,
    /// The picture, once read (None inside: there was none, or it didn't decode).
    cover: Option<Option<usize>>,
}

struct Album {
    title: String,
    artist: String,
    year: String,
    cover: Option<usize>,
    songs: Vec<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Songs,
    Albums,
    Album(usize),
    Artists,
    Artist(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sort {
    Title,
    Artist,
    Album,
    Time,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Repeat {
    Off,
    All,
    One,
}

struct Playing {
    song: usize,
    engine: Engine,
    /// Where in the song (ms) playing started, and where that is in the sound output.
    from: u64,
    base: u64,
    pos: u64,
    dur: u64,
    paused: bool,
    /// Started without sound (no sound output): timed by the clock.
    tick: u64,
    sound: bool,
}

pub enum MusicAction {
    None,
    Redraw,
    /// Open this song in the Sound Editor.
    Edit(Source, String),
}

pub struct MusicApp {
    songs: Vec<Song>,
    covers: Vec<Image>,
    /// The covers small, for the player bar (made when first needed).
    small: Vec<Option<Image>>,
    folders: Vec<FolderArt>,
    albums: Vec<Album>,
    artists: Vec<(String, Vec<usize>)>,
    groups_stale: bool,
    view: View,
    sort: (Sort, bool),
    /// The songs listed now, in order (Songs, an album's or an artist's).
    list: Vec<usize>,
    selected: Option<usize>,
    scroll: i32,
    body: Rect,
    playing: Option<Playing>,
    /// What plays: the order (song indices) and where in it.
    queue: Vec<usize>,
    qpos: usize,
    shuffle: bool,
    repeat: Repeat,
    volume: u32,
    /// Dragging: 1 = the position, 2 = the volume.
    dragging: u8,
    last_click: Option<(usize, u64)>,
    rng: u64,
    /// What the player bar shows (seconds), to redraw when it changes.
    shown_sec: u64,
    scanned: bool,
    tag_next: usize,
    /// Folder pictures still to look at (for songs without a cover of their own).
    art_pending: bool,
    message: Option<(String, u64)>,
}

impl MusicApp {
    pub fn new() -> MusicApp {
        MusicApp {
            songs: Vec::new(),
            covers: Vec::new(),
            small: Vec::new(),
            folders: Vec::new(),
            albums: Vec::new(),
            artists: Vec::new(),
            groups_stale: true,
            view: View::Songs,
            sort: (Sort::Artist, true),
            list: Vec::new(),
            selected: None,
            scroll: 0,
            body: Rect::new(0, 0, 0, 0),
            playing: None,
            queue: Vec::new(),
            qpos: 0,
            shuffle: false,
            repeat: Repeat::Off,
            volume: 80,
            dragging: 0,
            last_click: None,
            rng: unsafe { core::arch::x86_64::_rdtsc() } | 1,
            shown_sec: u64::MAX,
            scanned: false,
            tag_next: 0,
            art_pending: false,
            message: None,
        }
    }

    /// What's playing, for the window's title: "Title - Artist".
    pub fn title(&self) -> Option<String> {
        let p = self.playing.as_ref()?;
        let s = &self.songs[p.song];
        Some(if s.artist.is_empty() { format!("Music - {}", s.title) } else { format!("Music - {} - {}", s.title, s.artist) })
    }

    /// The songs have been looked for.
    pub fn scanned(&self) -> bool {
        self.scanned
    }

    /// Every song that can be played (for the Sound Editor's list): name, place, where.
    pub fn song_list(&self) -> Vec<(String, String, Source, u64)> {
        let mut v: Vec<(String, String, Source, u64)> = self
            .songs
            .iter()
            .filter(|s| s.problem.is_none())
            .map(|s| {
                let shown = if s.artist.is_empty() { s.title.clone() } else { format!("{} - {}", s.title, s.artist) };
                (shown, s.place.clone(), s.src.clone(), s.size)
            })
            .collect();
        v.sort_by(|a, b| crate::fs::natural_cmp(&a.0.to_ascii_lowercase(), &b.0.to_ascii_lowercase()));
        v
    }

    // ---- finding songs -------------------------------------------------------------------

    /// Find every song (when the window opens; what's playing carries on).
    pub fn opened(&mut self, storage: &mut Storage, usb: &mut Usb) {
        if self.scanned && self.playing.is_some() {
            return;
        }
        let mut songs = Vec::new();
        let mut folders: Vec<FolderArt> = Vec::new();
        // The demo songs on the boot partition.
        if let Ok(list) = crate::display::boot_folder(storage, usb, "music") {
            let fi = Self::folder(&mut folders, &list, |e| Source::Boot { entry: e.clone() });
            for e in list.into_iter().filter(|e| !e.is_dir && is_music(&e.name) && e.size > 0) {
                let size = e.size;
                songs.push(Song::new(e.name.clone(), String::from("Demo songs"), Source::Boot { entry: e }, size, fi));
            }
        }
        // Solon storage, everywhere but the Recycle bin and the hidden originals.
        if storage.library.is_some() {
            if let Ok(root) = storage.root(usb) {
                let mut stack = vec![(root, 0u32)];
                while let Some((dir, depth)) = stack.pop() {
                    let list = storage.list(usb, &dir).unwrap_or_default();
                    let d2 = dir.clone();
                    let fi = Self::folder(&mut folders, &list, |e| Source::Library { dir: d2.clone(), entry: e.clone() });
                    for e in list {
                        if e.is_dir {
                            let hidden = depth == 0 && (e.name.eq_ignore_ascii_case(crate::library::BIN) || e.name.eq_ignore_ascii_case("Originals"));
                            if !hidden && depth < 6 {
                                stack.push((e, depth + 1));
                            }
                        } else if is_music(&e.name) && e.size > 0 {
                            let size = e.size;
                            songs.push(Song::new(e.name.clone(), String::from("Solon storage"), Source::Library { dir: dir.clone(), entry: e }, size, fi));
                        }
                    }
                }
            }
        }
        // USB drives and phones (a few folders deep, and not forever).
        for s in usb.sources() {
            let Some(root) = usb.source_root(s.drive, s.vol) else { continue };
            let mut stack = vec![(root, 0u32)];
            let mut seen = 0;
            while let Some((dir, depth)) = stack.pop() {
                let Ok(list) = usb.list(s.drive, s.vol, &dir) else { continue };
                seen += list.len();
                if seen > 20_000 {
                    break;
                }
                let (drive, vol) = (s.drive, s.vol);
                let fi = Self::folder(&mut folders, &list, |e| Source::Usb { drive, vol, entry: e.clone() });
                for e in list {
                    if e.is_dir {
                        let lower = e.name.to_ascii_lowercase();
                        let wanted = !s.phone || depth > 0 || ["music", "download", "downloads", "audio", "sounds", "ringtones", "recordings", "podcasts"].contains(&lower.as_str());
                        if depth < 6 && wanted && !lower.starts_with('.') && lower != "android" {
                            stack.push((e, depth + 1));
                        }
                    } else if is_music(&e.name) && e.size > 0 {
                        let size = e.size;
                        songs.push(Song::new(e.name.clone(), s.label.clone(), Source::Usb { drive, vol, entry: e }, size, fi));
                    }
                }
            }
        }
        // Keep what was learned about songs that are still here.
        for s in songs.iter_mut() {
            if let Some(old) = self.songs.iter().find(|o| o.name == s.name && o.size == s.size && o.place == s.place && o.tagged) {
                s.title = old.title.clone();
                s.artist = old.artist.clone();
                s.album = old.album.clone();
                s.year = old.year.clone();
                s.track = old.track;
                s.disc = old.disc;
                s.dur_ms = old.dur_ms;
                s.problem = old.problem.clone();
                s.tagged = true;
                s.cover = None; // covers are read again (the folder list is new)
                s.tagged = old.cover.is_none();
            }
        }
        kprintln!("music: {} songs found", songs.len());
        self.songs = songs;
        self.folders = folders;
        self.covers.clear();
        self.small.clear();
        self.playing = None;
        self.queue.clear();
        self.selected = None;
        self.scroll = 0;
        self.view = View::Songs;
        self.tag_next = 0;
        self.art_pending = true;
        self.groups_stale = true;
        self.scanned = true;
        self.relist();
    }

    /// Note a folder of songs (with the picture in it that could be their cover).
    fn folder(folders: &mut Vec<FolderArt>, list: &[DirEntry], src: impl Fn(&DirEntry) -> Source) -> Option<usize> {
        if !list.iter().any(|e| !e.is_dir && is_music(&e.name)) {
            return None;
        }
        let pick = |names: &[&str]| list.iter().find(|e| !e.is_dir && names.iter().any(|n| e.name.eq_ignore_ascii_case(n)));
        let art = pick(&["folder.jpg", "cover.jpg", "front.jpg", "album.jpg", "folder.png", "cover.png"])
            .or_else(|| list.iter().find(|e| !e.is_dir && e.name.to_ascii_lowercase().starts_with("albumart") && e.name.to_ascii_lowercase().ends_with("large.jpg")))
            .or_else(|| list.iter().find(|e| !e.is_dir && e.name.to_ascii_lowercase().starts_with("albumart") && e.name.to_ascii_lowercase().ends_with(".jpg")));
        folders.push(FolderArt { art: art.map(&src), cover: None });
        Some(folders.len() - 1)
    }

    /// Read the tags of the next few songs (a little each tick, so the window stays quick).
    fn read_tags(&mut self, io: &mut LibIo) -> bool {
        let t0 = crate::cpu::now_us();
        let mut changed = false;
        while self.tag_next < self.songs.len() && crate::cpu::now_us() - t0 < 15_000 {
            let i = self.tag_next;
            self.tag_next += 1;
            if self.songs[i].tagged {
                continue;
            }
            let src = self.songs[i].src.clone();
            let size = self.songs[i].size;
            let tags = {
                let mut read = |off: u64, len: usize| io.read(&src, off, len);
                crate::media::tags::read(&mut read, size)
            };
            // The length and whether it can be played: the container's header.
            let (dur, problem) = {
                let mut read = |off: u64, len: usize| io.read(&src, off, len);
                match crate::media::open_demux(&mut read, size) {
                    Ok(d) => {
                        let info = d.info();
                        match &info.audio {
                            Some(a) => match crate::media::dec::audio(a) {
                                Ok(_) => (Some(info.duration_ms), None),
                                Err(e) => (Some(info.duration_ms), Some(e)),
                            },
                            None => (None, Some(String::from("This file has no sound"))),
                        }
                    }
                    Err(e) => (None, Some(if self.songs[i].name.to_ascii_lowercase().ends_with(".wma") { String::from("Windows Media songs can't be played yet") } else { e })),
                }
            };
            let cover = tags.cover.as_ref().and_then(|b| self.add_cover(b));
            let s = &mut self.songs[i];
            if let Some(t) = tags.title {
                s.title = t;
            }
            if let Some(a) = tags.artist.or(tags.album_artist.clone()) {
                s.artist = a;
            }
            if let Some(a) = tags.album {
                s.album = a;
            }
            s.year = tags.year.unwrap_or_default();
            s.track = tags.track.unwrap_or(0);
            s.disc = tags.disc.unwrap_or(0);
            s.dur_ms = dur;
            s.problem = problem;
            s.cover = cover;
            s.tagged = true;
            changed = true;
        }
        // Folder pictures for songs without a cover of their own (once every song's tags are in).
        if self.tag_next < self.songs.len() || !self.art_pending {
            if changed {
                self.groups_stale = true;
                self.relist();
            }
            return changed;
        }
        let mut wanting = vec![false; self.folders.len()];
        for s in self.songs.iter().filter(|s| s.cover.is_none()) {
            if let Some(fi) = s.folder {
                wanting[fi] = true;
            }
        }
        self.art_pending = false;
        for fi in 0..self.folders.len() {
            if self.folders[fi].cover.is_some() {
                continue;
            }
            if !wanting[fi] {
                self.folders[fi].cover = Some(None);
                continue;
            }
            if crate::cpu::now_us() - t0 > 30_000 {
                self.art_pending = true;
                break;
            }
            let c = match self.folders[fi].art.clone() {
                Some(src) => io.read_all(&src).and_then(|b| self.add_cover(&b)),
                None => None,
            };
            self.folders[fi].cover = Some(c);
            if c.is_some() {
                for s in self.songs.iter_mut().filter(|s| s.folder == Some(fi) && s.cover.is_none()) {
                    s.cover = c;
                }
                changed = true;
            }
        }
        if changed {
            self.groups_stale = true;
            self.relist();
        }
        changed
    }

    /// Decode a cover picture (once per picture: the same cover in every song of an album is
    /// stored once).
    fn add_cover(&mut self, bytes: &[u8]) -> Option<usize> {
        let img = crate::image::decode(bytes).ok()?;
        let img = img.shrink_to_fit(COVER, COVER);
        if let Some(i) = self.covers.iter().position(|c| c.w == img.w && c.h == img.h && c.px == img.px) {
            return Some(i);
        }
        self.covers.push(img);
        self.small.push(None);
        Some(self.covers.len() - 1)
    }

    /// Albums and artists, from the songs' tags.
    fn regroup(&mut self) {
        if !self.groups_stale {
            return;
        }
        self.groups_stale = false;
        let mut albums: Vec<Album> = Vec::new();
        for (i, s) in self.songs.iter().enumerate() {
            let title = String::from(s.album_or_unknown());
            match albums.iter_mut().find(|a| a.title == title && (a.artist == s.artist || s.album.is_empty() == false && a.title == title)) {
                Some(a) => {
                    a.songs.push(i);
                    if a.cover.is_none() {
                        a.cover = s.cover;
                    }
                    if a.artist != s.artist && !a.artist.is_empty() && !s.artist.is_empty() {
                        a.artist = String::from("Various artists");
                    }
                }
                None => albums.push(Album { title, artist: s.artist.clone(), year: s.year.clone(), cover: s.cover, songs: vec![i] }),
            }
        }
        for a in albums.iter_mut() {
            let songs = &self.songs;
            a.songs.sort_by(|&x, &y| (songs[x].disc, songs[x].track).cmp(&(songs[y].disc, songs[y].track)).then_with(|| crate::fs::natural_cmp(&songs[x].name, &songs[y].name)));
        }
        albums.sort_by(|a, b| crate::fs::natural_cmp(&a.title.to_ascii_lowercase(), &b.title.to_ascii_lowercase()));
        let mut artists: Vec<(String, Vec<usize>)> = Vec::new();
        for (i, s) in self.songs.iter().enumerate() {
            let name = String::from(s.artist_or_unknown());
            match artists.iter_mut().find(|a| a.0 == name) {
                Some(a) => a.1.push(i),
                None => artists.push((name, vec![i])),
            }
        }
        artists.sort_by(|a, b| crate::fs::natural_cmp(&a.0.to_ascii_lowercase(), &b.0.to_ascii_lowercase()));
        for a in artists.iter_mut() {
            let songs = &self.songs;
            a.1.sort_by(|&x, &y| crate::fs::natural_cmp(&songs[x].album, &songs[y].album).then((songs[x].disc, songs[x].track).cmp(&(songs[y].disc, songs[y].track))));
        }
        self.albums = albums;
        self.artists = artists;
        if let View::Album(i) = self.view {
            if i >= self.albums.len() {
                self.view = View::Albums;
            }
        }
        if let View::Artist(i) = self.view {
            if i >= self.artists.len() {
                self.view = View::Artists;
            }
        }
    }

    /// The song list for the view, sorted.
    fn relist(&mut self) {
        self.regroup();
        self.list = match self.view {
            View::Songs => {
                let mut l: Vec<usize> = (0..self.songs.len()).collect();
                let s = &self.songs;
                let lower = |t: &str| t.to_ascii_lowercase();
                l.sort_by(|&a, &b| {
                    let (x, y) = (&s[a], &s[b]);
                    let o = match self.sort.0 {
                        Sort::Title => crate::fs::natural_cmp(&lower(&x.title), &lower(&y.title)),
                        Sort::Artist => crate::fs::natural_cmp(&lower(x.artist_or_unknown()), &lower(y.artist_or_unknown()))
                            .then_with(|| crate::fs::natural_cmp(&lower(&x.album), &lower(&y.album)))
                            .then((x.disc, x.track).cmp(&(y.disc, y.track)))
                            .then_with(|| crate::fs::natural_cmp(&lower(&x.title), &lower(&y.title))),
                        Sort::Album => crate::fs::natural_cmp(&lower(x.album_or_unknown()), &lower(y.album_or_unknown())).then((x.disc, x.track).cmp(&(y.disc, y.track))),
                        Sort::Time => x.dur_ms.unwrap_or(0).cmp(&y.dur_ms.unwrap_or(0)),
                    };
                    if self.sort.1 {
                        o
                    } else {
                        o.reverse()
                    }
                });
                l
            }
            View::Album(i) => self.albums.get(i).map(|a| a.songs.clone()).unwrap_or_default(),
            View::Artist(i) => self.artists.get(i).map(|a| a.1.clone()).unwrap_or_default(),
            View::Albums | View::Artists => Vec::new(),
        };
    }

    // ---- playing --------------------------------------------------------------------------

    /// Start playing `queue` from position `qpos`.
    fn play_queue(&mut self, queue: Vec<usize>, qpos: usize, io: &mut LibIo) {
        self.queue = queue;
        self.qpos = qpos;
        if self.shuffle && !self.queue.is_empty() {
            // The chosen song first, the rest in a random order.
            let first = self.queue.remove(qpos.min(self.queue.len() - 1));
            self.shuffle_queue();
            self.queue.insert(0, first);
            self.qpos = 0;
        }
        if self.start_song(io, true) {
            self.queue_sound(io);
        }
    }

    fn shuffle_queue(&mut self) {
        for i in (1..self.queue.len()).rev() {
            self.rng ^= self.rng << 13;
            self.rng ^= self.rng >> 7;
            self.rng ^= self.rng << 17;
            let j = (self.rng % (i as u64 + 1)) as usize;
            self.queue.swap(i, j);
        }
    }

    /// Open the song at `qpos` and play it from the start. `fresh`: start the sound output
    /// over (else it carries straight on from the last song).
    fn start_song(&mut self, io: &mut LibIo, fresh: bool) -> bool {
        // Skip songs that can't be played (at most once round the queue).
        for _ in 0..self.queue.len().max(1) {
            let Some(&i) = self.queue.get(self.qpos) else { break };
            let s = &self.songs[i];
            let src = s.src.clone();
            let size = s.size;
            let opened = {
                let mut read = |off: u64, len: usize| io.read(&src, off, len);
                Engine::open(&mut read, size)
            };
            match opened {
                Ok(e) if e.has_sound() => {
                    let dur = e.duration_ms;
                    if self.songs[i].dur_ms.is_none() {
                        self.songs[i].dur_ms = Some(dur);
                    }
                    let sound = crate::hda::available();
                    let base = if sound {
                        if fresh || crate::hda::owner() != crate::hda::MUSIC {
                            crate::hda::start(crate::hda::MUSIC);
                        }
                        crate::hda::write_pos()
                    } else {
                        0
                    };
                    kprintln!("music: playing \"{}\" ({} ms)", self.songs[i].name, dur);
                    self.playing = Some(Playing { song: i, engine: e, from: 0, base, pos: 0, dur, paused: false, tick: TICKS.load(Relaxed), sound });
                    self.selected = Some(i);
                    return true;
                }
                Ok(e) => self.songs[i].problem = Some(e.sound_problem.clone().unwrap_or_else(|| String::from("This file has no sound"))),
                Err(e) => self.songs[i].problem = Some(e),
            }
            self.qpos += 1;
            if self.qpos >= self.queue.len() {
                if self.repeat == Repeat::Off {
                    break;
                }
                self.qpos = 0;
            }
        }
        self.stop();
        false
    }

    fn stop(&mut self) {
        if self.playing.take().is_some_and(|p| p.sound && !p.paused) {
            crate::hda::stop(crate::hda::MUSIC);
        }
    }

    fn toggle(&mut self, io: &mut LibIo) {
        let Some(p) = self.playing.as_mut() else {
            // Nothing playing: play the selected song (or the list from the top).
            let list = self.list.clone();
            let at = self.selected.and_then(|s| list.iter().position(|&x| x == s)).unwrap_or(0);
            if !list.is_empty() {
                self.play_queue(list, at, io);
            }
            return;
        };
        if p.paused {
            p.paused = false;
            p.from = p.pos;
            p.tick = TICKS.load(Relaxed);
            if p.sound {
                crate::hda::start(crate::hda::MUSIC);
                p.base = crate::hda::write_pos();
                self.queue_sound(io);
            }
        } else {
            p.paused = true;
            if p.sound {
                crate::hda::stop(crate::hda::MUSIC);
            }
        }
    }

    fn seek(&mut self, ms: u64, io: &mut LibIo) {
        let Some(p) = self.playing.as_mut() else { return };
        p.pos = ms.min(p.dur);
        p.from = p.pos;
        p.tick = TICKS.load(Relaxed);
        if !p.paused && p.sound {
            crate::hda::stop(crate::hda::MUSIC);
            crate::hda::start(crate::hda::MUSIC);
            p.base = crate::hda::write_pos();
            self.queue_sound(io);
        }
    }

    fn next(&mut self, io: &mut LibIo, by_user: bool) {
        if self.queue.is_empty() {
            return;
        }
        if self.repeat == Repeat::One && !by_user {
            // The same song again.
        } else if self.qpos + 1 < self.queue.len() {
            self.qpos += 1;
        } else if self.repeat != Repeat::Off || by_user {
            self.qpos = 0;
            if self.shuffle {
                self.shuffle_queue();
            }
        } else {
            self.stop();
            return;
        }
        let was_paused = self.playing.as_ref().is_some_and(|p| p.paused);
        if self.start_song(io, by_user || was_paused) {
            if was_paused {
                self.toggle(io); // stay paused
            } else {
                self.queue_sound(io);
            }
        }
    }

    fn previous(&mut self, io: &mut LibIo) {
        // A few seconds in: back to the start of this song; else the one before.
        if self.playing.as_ref().is_some_and(|p| p.pos > 3000) || self.queue.is_empty() {
            self.seek(0, io);
            return;
        }
        self.qpos = if self.qpos == 0 { self.queue.len() - 1 } else { self.qpos - 1 };
        if self.start_song(io, true) {
            self.queue_sound(io);
        }
    }

    /// Top the sound output up; one song runs straight into the next.
    fn queue_sound(&mut self, io: &mut LibIo) {
        for _ in 0..2 {
            let Some(p) = self.playing.as_mut() else { return };
            if p.paused || !p.sound {
                return;
            }
            let want = crate::hda::wanted(crate::hda::MUSIC);
            if want == 0 {
                return;
            }
            let at = crate::hda::write_pos();
            let start = p.from * 48 + at.saturating_sub(p.base);
            let src = self.songs[p.song].src.clone();
            if let Some(end) = p.engine.sound_end() {
                if start >= end {
                    // This song is all queued: the next one starts right after it.
                    if !self.advance_queue() {
                        return;
                    }
                    if !self.start_song(io, false) {
                        return;
                    }
                    continue;
                }
            }
            let n = match p.engine.sound_end() {
                Some(end) => want.min((end - start) as usize),
                None => want,
            };
            if n < 1024 && p.engine.sound_end().is_none() && start > 0 {
                return; // not worth it yet
            }
            let mut read = |off: u64, len: usize| io.read(&src, off, len);
            let mut pcm = p.engine.sound(&mut read, start, n.max(1));
            let g = (self.volume * self.volume) as i32; // 0..10000: a curve that sounds even
            for s in pcm.iter_mut() {
                *s = (*s as i32 * g / 10_000) as i16;
            }
            crate::hda::queue(crate::hda::MUSIC, &pcm);
            return;
        }
    }

    /// Move to the next song in the queue (after one ends by itself). False at the end.
    fn advance_queue(&mut self) -> bool {
        if self.repeat == Repeat::One {
            return true;
        }
        if self.qpos + 1 < self.queue.len() {
            self.qpos += 1;
            return true;
        }
        if self.repeat == Repeat::All && !self.queue.is_empty() {
            self.qpos = 0;
            if self.shuffle {
                self.shuffle_queue();
            }
            return true;
        }
        false
    }

    pub fn tick(&mut self, now: u64, storage: &mut Storage, usb: &mut Usb, visible: bool) -> MusicAction {
        let mut io = LibIo { storage, usb };
        let mut redraw = false;
        if visible && self.scanned && (self.tag_next < self.songs.len() || self.art_pending) {
            redraw |= self.read_tags(&mut io);
        }
        // Another app took the sound: pause.
        if self.playing.as_ref().is_some_and(|p| p.sound && !p.paused && crate::hda::owner() != crate::hda::MUSIC) {
            if let Some(p) = self.playing.as_mut() {
                p.paused = true;
            }
            redraw = true;
        }
        if let Some(p) = self.playing.as_mut() {
            if !p.paused {
                let elapsed = if p.sound { crate::hda::played().saturating_sub(p.base) * 1000 / 48_000 } else { now.saturating_sub(p.tick) * 1000 / HZ };
                if self.dragging != 1 {
                    p.pos = (p.from + elapsed).min(p.dur);
                }
            }
        }
        self.queue_sound(&mut io);
        // The end of the last song (or a song played without sound): stop or go on.
        let ended = self.playing.as_ref().is_some_and(|p| !p.paused && ((p.sound && p.engine.sound_end().is_some_and(|e| crate::hda::played().saturating_sub(p.base) + p.from * 48 >= e)) || (!p.sound && p.pos >= p.dur)));
        if ended {
            let last = self.qpos + 1 >= self.queue.len() && self.repeat == Repeat::Off;
            if last {
                self.stop();
                redraw = true;
            } else if !self.playing.as_ref().is_some_and(|p| p.sound) {
                self.next(&mut io, false);
                redraw = true;
            }
        }
        if let Some((_, until)) = &self.message {
            if now >= *until {
                self.message = None;
                redraw = true;
            }
        }
        let sec = self.playing.as_ref().map(|p| p.pos / 1000 + (p.song as u64) * 100_000).unwrap_or(u64::MAX - 1);
        if sec != self.shown_sec {
            self.shown_sec = sec;
            redraw = true;
        }
        if redraw && (visible || self.playing.is_some()) {
            MusicAction::Redraw
        } else {
            MusicAction::None
        }
    }

    pub fn closed(&mut self) {
        self.stop();
    }

    // ---- layout ----------------------------------------------------------------------------

    fn tabs(r: Rect) -> [(Rect, &'static str); 3] {
        let mut x = r.x + 16;
        let mut out = [(Rect::new(0, 0, 0, 0), ""); 3];
        for (k, name) in ["Songs", "Albums", "Artists"].iter().enumerate() {
            let w = text_width(name, Size::Medium, true) + 32;
            out[k] = (Rect::new(x, r.y + 10, w, 36), name);
            x += w + 8;
        }
        out
    }
    fn edit_button(r: Rect) -> Rect {
        Rect::new(r.x + r.w - 16 - 230, r.y + 10, 230, 36)
    }
    fn list_area(r: Rect) -> Rect {
        Rect::new(r.x, r.y + TOP_H, r.w, r.h - TOP_H - BAR_H)
    }
    fn bar(r: Rect) -> Rect {
        Rect::new(r.x, r.y + r.h - BAR_H, r.w, BAR_H)
    }
    /// (previous, play, next, seek bar, shuffle, repeat, volume bar)
    fn controls(r: Rect) -> (Rect, Rect, Rect, Rect, Rect, Rect, Rect) {
        let b = Self::bar(r);
        let cx = b.x + b.w / 2;
        let play = Rect::new(cx - 26, b.y + 10, 52, 52);
        let prev = Rect::new(play.x - 60, b.y + 16, 44, 40);
        let next = Rect::new(play.x + play.w + 16, b.y + 16, 44, 40);
        let seek = Rect::new(cx - (b.w / 4).min(300), b.y + 74, (b.w / 2).min(600), 8);
        let vol = Rect::new(b.x + b.w - 20 - 120, b.y + 34, 120, 10);
        let repeat = Rect::new(vol.x - 50 - 44, b.y + 20, 44, 40);
        let shuffle = Rect::new(repeat.x - 52, b.y + 20, 44, 40);
        (prev, play, next, seek, shuffle, repeat, vol)
    }
    /// Columns of the song list: (x of title, artist, album, time).
    fn columns(a: Rect, numbered: bool) -> (i32, i32, i32, i32) {
        // (Room before the title for the speaker that marks the song playing.)
        let x0 = a.x + if numbered { 64 } else { 44 };
        let time = a.x + a.w - 90;
        let span = time - x0;
        (x0, x0 + span * 40 / 100, x0 + span * 70 / 100, time)
    }
    /// Where the list (or grid) itself starts, below any header.
    fn content(&self, r: Rect) -> Rect {
        let a = Self::list_area(r);
        match self.view {
            View::Songs => Rect::new(a.x, a.y + HEAD_H, a.w, a.h - HEAD_H),
            View::Album(_) | View::Artist(_) => Rect::new(a.x, a.y + 210, a.w, a.h - 210),
            _ => a,
        }
    }
    fn album_tile(&self, a: Rect, i: usize) -> Rect {
        let cols = ((a.w - 20) / TILE_W).max(1);
        let tw = (a.w - 20) / cols;
        Rect::new(a.x + 10 + (i as i32 % cols) * tw + (tw - TILE_W) / 2, a.y + 12 + (i as i32 / cols) * TILE_H - self.scroll, TILE_W - 12, TILE_H - 12)
    }
    fn max_scroll(&self, r: Rect) -> i32 {
        let c = self.content(r);
        let h = match self.view {
            View::Albums => {
                let cols = ((c.w - 20) / TILE_W).max(1);
                (self.albums.len() as i32 + cols - 1) / cols * TILE_H + 24
            }
            View::Artists => self.artists.len() as i32 * ROW_H + 12,
            _ => self.list.len() as i32 * ROW_H + 12,
        };
        (h - c.h).max(0)
    }

    // ---- drawing ------------------------------------------------------------------------------

    fn cover_or_note(&mut self, c: &mut Canvas, cover: Option<usize>, x: i32, y: i32, size: i32, seed: &str) {
        match cover {
            Some(i) if i < self.covers.len() => {
                let img = if size as usize >= COVER - 10 {
                    &self.covers[i]
                } else {
                    if self.small[i].as_ref().is_none_or(|s| s.w.max(s.h) != size as usize) {
                        self.small[i] = Some(self.covers[i].shrink_to_fit(size as usize, size as usize));
                    }
                    self.small[i].as_ref().unwrap()
                };
                c.fill_rect(x, y, size, size, rgb(20, 20, 24));
                c.draw_image(img, x + (size - img.w as i32) / 2, y + (size - img.h as i32) / 2);
            }
            _ => {
                // No cover: a note on a colour of its own (from the name).
                let h = seed.bytes().fold(5381u32, |a, b| a.wrapping_mul(33) ^ b as u32);
                let palette = [rgb(208, 46, 106), rgb(52, 120, 220), rgb(230, 126, 34), rgb(39, 174, 96), rgb(142, 68, 173), rgb(22, 160, 133), rgb(192, 57, 43), rgb(41, 128, 185)];
                c.round_rect(x, y, size, size, (size / 12).max(3), palette[(h % 8) as usize]);
                note(c, x + size / 2, y + size / 2, size);
            }
        }
    }

    pub fn draw(&mut self, c: &mut Canvas, r: Rect) {
        self.body = r;
        self.regroup();
        c.fill_rect(r.x, r.y, r.w, r.h, BG);
        // Top: the views, and the Sound Editor button.
        c.fill_rect(r.x, r.y, r.w, TOP_H, PANEL);
        c.fill_rect(r.x, r.y + TOP_H - 1, r.w, 1, LINE);
        let current = match self.view {
            View::Songs => 0,
            View::Albums | View::Album(_) => 1,
            View::Artists | View::Artist(_) => 2,
        };
        for (k, (t, name)) in Self::tabs(r).iter().enumerate() {
            let on = k == current;
            c.round_rect(t.x, t.y, t.w, t.h, 18, if on { ACCENT } else { PANEL });
            c.text_centered(t.x + t.w / 2, t.y + 8, name, Size::Medium, true, if on { WHITE } else { INK });
        }
        let eb = Self::edit_button(r);
        if r.w > 700 {
            let can = self.selected.is_some() || self.playing.is_some();
            c.round_rect(eb.x, eb.y, eb.w, eb.h, 8, if can { WHITE } else { PANEL });
            c.text_centered(eb.x + eb.w / 2, eb.y + 9, "Open in Sound Editor", Size::Small, true, if can { INK } else { DIM });
        }
        let a = Self::list_area(r);
        c.set_clip(a);
        if self.songs.is_empty() {
            let msg = if self.scanned { "No music yet." } else { "Looking for music..." };
            c.text_centered(a.x + a.w / 2, a.y + a.h / 2 - 30, msg, Size::Medium, true, INK);
            c.text_centered(a.x + a.w / 2, a.y + a.h / 2 + 4, "Songs in Solon storage, on USB drives and on phones show up here.", Size::Small, false, DIM);
        } else {
            match self.view {
                View::Songs => self.draw_songs(c, r, a, false),
                View::Albums => self.draw_albums(c, a),
                View::Artists => self.draw_artists(c, a),
                View::Album(i) => {
                    let (title, artist, year, cover, n) = {
                        let al = &self.albums[i.min(self.albums.len() - 1)];
                        (al.title.clone(), al.artist.clone(), al.year.clone(), al.cover, al.songs.len())
                    };
                    self.draw_group_header(c, a, &title, &artist, &year, cover, n);
                    self.draw_songs(c, r, Rect::new(a.x, a.y + 210, a.w, a.h - 210), true);
                }
                View::Artist(i) => {
                    let (name, n, cover) = {
                        let ar = &self.artists[i.min(self.artists.len() - 1)];
                        (ar.0.clone(), ar.1.len(), ar.1.iter().find_map(|&s| self.songs[s].cover))
                    };
                    self.draw_group_header(c, a, &name, "", "", cover, n);
                    self.draw_songs(c, r, Rect::new(a.x, a.y + 210, a.w, a.h - 210), true);
                }
            }
        }
        c.reset_clip();
        self.draw_bar(c, r);
        if let Some((m, _)) = &self.message {
            let w = text_width(m, Size::Small, true) + 40;
            c.round_rect(r.x + (r.w - w) / 2, a.y + a.h - 50, w, 34, 17, rgb(36, 40, 50));
            c.text_centered(r.x + r.w / 2, a.y + a.h - 41, m, Size::Small, true, WHITE);
        }
    }

    fn draw_group_header(&mut self, c: &mut Canvas, a: Rect, title: &str, artist: &str, year: &str, cover: Option<usize>, n: usize) {
        c.fill_rect(a.x, a.y, a.w, 210, BG);
        self.cover_or_note(c, cover, a.x + 24, a.y + 20, COVER as i32, title);
        let x = a.x + 24 + COVER as i32 + 28;
        let back = Rect::new(x, a.y + 20, 90, 34);
        c.round_rect(back.x, back.y, back.w, back.h, 8, PANEL);
        c.text_centered(back.x + back.w / 2, back.y + 8, "< Back", Size::Small, true, INK);
        c.text(x, a.y + 70, &ellipsize(title, a.w - (x - a.x) - 24, Size::Large, true), Size::Large, true, INK);
        let mut info = String::from(artist);
        if !year.is_empty() {
            if !info.is_empty() {
                info.push_str("  |  ");
            }
            info.push_str(year);
        }
        if !info.is_empty() {
            info.push_str("  |  ");
        }
        info.push_str(&format!("{} song{}", n, if n == 1 { "" } else { "s" }));
        c.text(x, a.y + 114, &info, Size::Small, false, DIM);
        let play = Rect::new(x, a.y + 148, 120, 40);
        c.round_rect(play.x, play.y, play.w, play.h, 20, ACCENT);
        c.triangle([(play.x + 22, play.y + 11), (play.x + 22, play.y + 29), (play.x + 38, play.y + 20)], WHITE);
        c.text(play.x + 48, play.y + 11, "Play", Size::Small, true, WHITE);
        c.fill_rect(a.x, a.y + 209, a.w, 1, LINE);
    }

    fn draw_songs(&mut self, c: &mut Canvas, r: Rect, a: Rect, numbered: bool) {
        let (cx, ax, bx, tx) = Self::columns(a, numbered);
        let mut top = a.y;
        if !numbered {
            // Column headers (click to sort).
            c.fill_rect(a.x, a.y, a.w, HEAD_H, BG);
            for (x, name, s) in [(cx, "Title", Sort::Title), (ax, "Artist", Sort::Artist), (bx, "Album", Sort::Album), (tx, "Time", Sort::Time)] {
                let on = self.sort.0 == s;
                c.text(x, a.y + 9, name, Size::Small, true, if on { ACCENT } else { DIM });
                if on {
                    let w = text_width(name, Size::Small, true);
                    let (ax2, ay2) = (x + w + 8, a.y + 16);
                    if self.sort.1 {
                        c.triangle([(ax2, ay2 + 3), (ax2 + 8, ay2 + 3), (ax2 + 4, ay2 - 3)], ACCENT);
                    } else {
                        c.triangle([(ax2, ay2 - 3), (ax2 + 8, ay2 - 3), (ax2 + 4, ay2 + 3)], ACCENT);
                    }
                }
            }
            c.fill_rect(a.x, a.y + HEAD_H - 1, a.w, 1, LINE);
            top += HEAD_H;
        }
        let area = Rect::new(a.x, top, a.w, a.y + a.h - top);
        c.set_clip(area);
        let playing = self.playing.as_ref().map(|p| p.song);
        for (k, &i) in self.list.iter().enumerate() {
            let y = top + k as i32 * ROW_H - self.scroll;
            if y + ROW_H < top || y > a.y + a.h {
                continue;
            }
            let s = &self.songs[i];
            if self.selected == Some(i) {
                c.fill_rect(a.x, y, a.w, ROW_H, SEL);
            } else if k % 2 == 1 {
                c.fill_rect(a.x, y, a.w, ROW_H, rgb(246, 246, 249));
            }
            let fg = if s.problem.is_some() { DIM } else { INK };
            let is_playing = playing == Some(i);
            if numbered {
                let n = if s.track > 0 { format!("{}", s.track) } else { format!("{}", k + 1) };
                c.text(a.x + 24, y + 10, &n, Size::Small, false, DIM);
            }
            if is_playing {
                // A little speaker before the title.
                let (sx, sy) = (cx - 22, y + ROW_H / 2);
                c.fill_rect(sx, sy - 3, 4, 6, ACCENT);
                c.triangle([(sx + 3, sy - 3), (sx + 9, sy - 8), (sx + 9, sy + 8)], ACCENT);
                c.triangle([(sx + 3, sy - 3), (sx + 9, sy + 8), (sx + 3, sy + 3)], ACCENT);
            }
            let title_c = if is_playing { ACCENT } else { fg };
            c.text(cx, y + 10, &ellipsize(&s.title, ax - cx - 16, Size::Small, true), Size::Small, true, title_c);
            c.text(ax, y + 10, &ellipsize(s.artist_or_unknown(), bx - ax - 16, Size::Small, false), Size::Small, false, fg);
            let album = match &s.problem {
                Some(p) => p.clone(),
                None => String::from(s.album_or_unknown()),
            };
            c.text(bx, y + 10, &ellipsize(&album, tx - bx - 16, Size::Small, false), Size::Small, false, if s.problem.is_some() { ACCENT } else { fg });
            if let Some(d) = s.dur_ms {
                c.text(tx, y + 10, &time_text(d), Size::Small, false, DIM);
            }
        }
        c.set_clip(Self::list_area(r));
    }

    fn draw_albums(&mut self, c: &mut Canvas, a: Rect) {
        for i in 0..self.albums.len() {
            let t = self.album_tile(a, i);
            if t.y + t.h < a.y || t.y > a.y + a.h {
                continue;
            }
            let (title, artist, cover) = (self.albums[i].title.clone(), self.albums[i].artist.clone(), self.albums[i].cover);
            self.cover_or_note(c, cover, t.x + (t.w - COVER as i32) / 2, t.y, COVER as i32, &title);
            c.text(t.x + 4, t.y + COVER as i32 + 10, &ellipsize(&title, t.w - 8, Size::Small, true), Size::Small, true, INK);
            let artist = if artist.is_empty() { String::from("Unknown artist") } else { artist };
            c.text(t.x + 4, t.y + COVER as i32 + 32, &ellipsize(&artist, t.w - 8, Size::Small, false), Size::Small, false, DIM);
        }
    }

    fn draw_artists(&mut self, c: &mut Canvas, a: Rect) {
        for k in 0..self.artists.len() {
            let y = a.y + 6 + k as i32 * ROW_H - self.scroll;
            if y + ROW_H < a.y || y > a.y + a.h {
                continue;
            }
            if k % 2 == 1 {
                c.fill_rect(a.x, y, a.w, ROW_H, rgb(246, 246, 249));
            }
            let (name, n) = (&self.artists[k].0, self.artists[k].1.len());
            c.text(a.x + 24, y + 10, &ellipsize(name, a.w - 200, Size::Small, true), Size::Small, true, INK);
            c.text(a.x + a.w - 140, y + 10, &format!("{} song{}", n, if n == 1 { "" } else { "s" }), Size::Small, false, DIM);
        }
    }

    fn draw_bar(&mut self, c: &mut Canvas, r: Rect) {
        let b = Self::bar(r);
        c.fill_rect(b.x, b.y, b.w, b.h, PANEL);
        c.fill_rect(b.x, b.y, b.w, 1, LINE);
        let (prev, play, next, seek, shuffle, repeat, vol) = Self::controls(r);
        // The song: cover, title, artist.
        let info = self.playing.as_ref().map(|p| (p.song, p.pos, p.dur, p.paused));
        let shown = info.map(|i| i.0).or(self.selected);
        if let Some(i) = shown {
            let (title, artist, cover) = (self.songs[i].title.clone(), String::from(self.songs[i].artist_or_unknown()), self.songs[i].cover);
            self.cover_or_note(c, cover, b.x + 16, b.y + 14, 68, &title);
            let maxw = (prev.x - (b.x + 100) - 20).max(40);
            c.text(b.x + 100, b.y + 24, &ellipsize(&title, maxw, Size::Small, true), Size::Small, true, INK);
            c.text(b.x + 100, b.y + 48, &ellipsize(&artist, maxw, Size::Small, false), Size::Small, false, DIM);
        }
        // Previous, play/pause, next.
        let (px, py) = (prev.x + prev.w / 2, prev.y + prev.h / 2);
        c.fill_rect(px - 11, py - 9, 4, 18, INK);
        c.triangle([(px + 9, py - 9), (px + 9, py + 9), (px - 6, py)], INK);
        let (nx, ny) = (next.x + next.w / 2, next.y + next.h / 2);
        c.fill_rect(nx + 7, ny - 9, 4, 18, INK);
        c.triangle([(nx - 9, ny - 9), (nx - 9, ny + 9), (nx + 6, ny)], INK);
        c.circle(play.x + play.w / 2, play.y + play.h / 2, play.w / 2, ACCENT);
        let (cx, cy) = (play.x + play.w / 2, play.y + play.h / 2);
        if info.is_some_and(|i| !i.3) {
            c.round_rect(cx - 10, cy - 11, 7, 22, 2, WHITE);
            c.round_rect(cx + 3, cy - 11, 7, 22, 2, WHITE);
        } else {
            c.triangle([(cx - 7, cy - 12), (cx - 7, cy + 12), (cx + 13, cy)], WHITE);
        }
        // Position.
        let (pos, dur) = info.map(|i| (i.1, i.2)).unwrap_or((0, 0));
        c.round_rect(seek.x, seek.y, seek.w, seek.h, 4, LINE);
        let done = if dur > 0 { (seek.w as u64 * pos.min(dur) / dur) as i32 } else { 0 };
        c.round_rect(seek.x, seek.y, done.max(8), seek.h, 4, ACCENT);
        if info.is_some() {
            c.circle(seek.x + done, seek.y + seek.h / 2, 8, ACCENT);
        }
        let t = time_text(pos);
        c.text(seek.x - 14 - text_width(&t, Size::Small, false), seek.y - 5, &t, Size::Small, false, DIM);
        c.text(seek.x + seek.w + 14, seek.y - 5, &time_text(dur), Size::Small, false, DIM);
        // Shuffle and repeat (lit when on).
        for (rect, on, kind) in [(shuffle, self.shuffle, 0), (repeat, self.repeat != Repeat::Off, 1)] {
            if on {
                c.round_rect(rect.x, rect.y, rect.w, rect.h, 8, SEL);
            }
            let col = if on { ACCENT } else { DIM };
            let (x, y) = (rect.x + rect.w / 2, rect.y + rect.h / 2);
            if kind == 0 {
                // Two crossing arrows.
                c.line_thick(x - 12, y - 7, x + 8, y + 7, 3, col);
                c.line_thick(x - 12, y + 7, x + 8, y - 7, 3, col);
                c.triangle([(x + 6, y - 12), (x + 6, y - 2), (x + 14, y - 7)], col);
                c.triangle([(x + 6, y + 2), (x + 6, y + 12), (x + 14, y + 7)], col);
            } else {
                // A loop with an arrow ("1" inside when repeating one song).
                c.round_rect(x - 13, y - 9, 26, 18, 7, col);
                c.round_rect(x - 10, y - 6, 20, 12, 5, if on { SEL } else { PANEL });
                c.triangle([(x + 2, y - 14), (x + 2, y - 4), (x + 9, y - 9)], col);
                if self.repeat == Repeat::One {
                    c.text_centered(x, y - 8, "1", Size::Small, true, col);
                }
            }
        }
        // Volume: a speaker and a bar.
        let (sx, sy) = (vol.x - 30, vol.y + vol.h / 2);
        c.fill_rect(sx, sy - 4, 5, 8, INK);
        c.triangle([(sx + 4, sy - 4), (sx + 12, sy - 10), (sx + 12, sy + 10)], INK);
        c.triangle([(sx + 4, sy - 4), (sx + 12, sy + 10), (sx + 4, sy + 4)], INK);
        c.round_rect(vol.x, vol.y, vol.w, vol.h, 5, LINE);
        let vw = vol.w * self.volume as i32 / 100;
        c.round_rect(vol.x, vol.y, vw.max(6), vol.h, 5, INK);
        c.circle(vol.x + vw, vol.y + vol.h / 2, 8, INK);
    }

    // ---- input -------------------------------------------------------------------------------

    pub fn scroll_info(&self, r: Rect) -> Option<(Rect, i32, i32)> {
        let max = self.max_scroll(r);
        (max > 0).then(|| (self.content(r), max, self.scroll))
    }
    pub fn set_scroll(&mut self, v: i32) {
        self.scroll = v.clamp(0, self.max_scroll(self.body));
    }

    pub fn on_wheel(&mut self, wheel: i8, r: Rect) -> bool {
        let max = self.max_scroll(r);
        self.scroll = (self.scroll - wheel as i32 * 80).clamp(0, max);
        true
    }

    fn set_view(&mut self, v: View) {
        self.view = v;
        self.scroll = 0;
        self.relist();
    }

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect, now: u64, storage: &mut Storage, usb: &mut Usb) -> MusicAction {
        self.body = r;
        let mut io = LibIo { storage, usb };
        // The views.
        for (k, (t, _)) in Self::tabs(r).iter().enumerate() {
            if t.contains(x, y) {
                self.set_view([View::Songs, View::Albums, View::Artists][k]);
                return MusicAction::Redraw;
            }
        }
        if r.w > 700 && Self::edit_button(r).contains(x, y) {
            let s = self.playing.as_ref().map(|p| p.song).or(self.selected);
            if let Some(i) = s {
                if self.songs[i].problem.is_some() {
                    self.message = Some((String::from("This song can't be opened: Solon can't play it yet"), now + 3 * HZ));
                    return MusicAction::Redraw;
                }
                return MusicAction::Edit(self.songs[i].src.clone(), self.songs[i].name.clone());
            }
            return MusicAction::None;
        }
        // The player bar.
        let (prev, play, next, seek, shuffle, repeat, vol) = Self::controls(r);
        if Self::bar(r).contains(x, y) {
            if play.contains(x, y) {
                self.toggle(&mut io);
            } else if prev.contains(x, y) {
                self.previous(&mut io);
            } else if next.contains(x, y) {
                self.next(&mut io, true);
            } else if shuffle.contains(x, y) {
                self.shuffle = !self.shuffle;
                if self.shuffle && self.queue.len() > 1 {
                    // Keep the song playing; the rest in a random order after it.
                    let cur = self.queue.remove(self.qpos.min(self.queue.len() - 1));
                    self.shuffle_queue();
                    self.queue.insert(0, cur);
                    self.qpos = 0;
                } else if !self.shuffle {
                    // Back to the list's order, carrying on from the song playing.
                    let cur = self.playing.as_ref().map(|p| p.song);
                    self.queue = self.list.clone();
                    self.qpos = cur.and_then(|c| self.queue.iter().position(|&q| q == c)).unwrap_or(0);
                }
            } else if repeat.contains(x, y) {
                self.repeat = match self.repeat {
                    Repeat::Off => Repeat::All,
                    Repeat::All => Repeat::One,
                    Repeat::One => Repeat::Off,
                };
            } else if Rect::new(seek.x - 10, seek.y - 14, seek.w + 20, seek.h + 28).contains(x, y) && self.playing.is_some() {
                self.dragging = 1;
                self.drag_seek(x, r);
            } else if Rect::new(vol.x - 30, vol.y - 14, vol.w + 40, vol.h + 28).contains(x, y) {
                self.dragging = 2;
                self.volume = ((x - vol.x).clamp(0, vol.w) * 100 / vol.w.max(1)) as u32;
            }
            return MusicAction::Redraw;
        }
        let a = Self::list_area(r);
        if !a.contains(x, y) {
            return MusicAction::None;
        }
        match self.view {
            View::Albums => {
                for i in 0..self.albums.len() {
                    if self.album_tile(a, i).contains(x, y) {
                        self.set_view(View::Album(i));
                        return MusicAction::Redraw;
                    }
                }
                MusicAction::None
            }
            View::Artists => {
                let k = (y - a.y - 6 + self.scroll) / ROW_H;
                if k >= 0 && (k as usize) < self.artists.len() {
                    self.set_view(View::Artist(k as usize));
                    return MusicAction::Redraw;
                }
                MusicAction::None
            }
            View::Songs | View::Album(_) | View::Artist(_) => {
                let group = self.view != View::Songs;
                if group && y < a.y + 210 {
                    let x0 = a.x + 24 + COVER as i32 + 28;
                    if Rect::new(x0, a.y + 20, 90, 34).contains(x, y) {
                        self.set_view(if matches!(self.view, View::Album(_)) { View::Albums } else { View::Artists });
                        return MusicAction::Redraw;
                    }
                    if Rect::new(x0, a.y + 148, 120, 40).contains(x, y) && !self.list.is_empty() {
                        let list = self.list.clone();
                        self.play_queue(list, 0, &mut io);
                        return MusicAction::Redraw;
                    }
                    return MusicAction::None;
                }
                let top = if group { a.y + 210 } else { a.y + HEAD_H };
                if !group && y < top {
                    // A column header: sort by it (again: the other way round).
                    let (cx, ax, bx, tx) = Self::columns(a, false);
                    let s = if x >= tx { Sort::Time } else if x >= bx { Sort::Album } else if x >= ax { Sort::Artist } else if x >= cx - 20 { Sort::Title } else { return MusicAction::None };
                    self.sort = if self.sort.0 == s { (s, !self.sort.1) } else { (s, true) };
                    self.relist();
                    return MusicAction::Redraw;
                }
                let k = (y - top + self.scroll) / ROW_H;
                if k < 0 || k as usize >= self.list.len() {
                    return MusicAction::None;
                }
                let i = self.list[k as usize];
                let double = self.last_click.is_some_and(|(j, t)| j == i && now.saturating_sub(t) < 50);
                self.last_click = Some((i, now));
                self.selected = Some(i);
                if double {
                    if let Some(p) = &self.songs[i].problem {
                        self.message = Some((p.clone(), now + 4 * HZ));
                    } else {
                        let list = self.list.clone();
                        self.play_queue(list, k as usize, &mut io);
                    }
                }
                MusicAction::Redraw
            }
        }
    }

    fn drag_seek(&mut self, x: i32, r: Rect) {
        let (_, _, _, seek, _, _, _) = Self::controls(r);
        if let Some(p) = self.playing.as_mut() {
            p.pos = ((x - seek.x).max(0) as u64 * p.dur / seek.w.max(1) as u64).min(p.dur);
        }
    }

    pub fn on_drag(&mut self, x: i32, _y: i32, r: Rect) -> bool {
        match self.dragging {
            1 => {
                self.drag_seek(x, r);
                true
            }
            2 => {
                let (_, _, _, _, _, _, vol) = Self::controls(r);
                self.volume = ((x - vol.x).clamp(0, vol.w) * 100 / vol.w.max(1)) as u32;
                true
            }
            _ => false,
        }
    }

    pub fn on_release(&mut self, storage: &mut Storage, usb: &mut Usb) -> bool {
        let d = self.dragging;
        self.dragging = 0;
        if d == 1 {
            let mut io = LibIo { storage, usb };
            let pos = self.playing.as_ref().map(|p| p.pos).unwrap_or(0);
            self.seek(pos, &mut io);
        }
        d != 0
    }

    pub fn on_key(&mut self, sc: u8, storage: &mut Storage, usb: &mut Usb) -> bool {
        if sc & 0x80 != 0 {
            return false;
        }
        let mut io = LibIo { storage, usb };
        match sc {
            0x39 => self.toggle(&mut io), // space
            0x1C => {
                // Enter: play the selected song.
                if let Some(k) = self.selected.and_then(|s| self.list.iter().position(|&x| x == s)) {
                    let list = self.list.clone();
                    self.play_queue(list, k, &mut io);
                } else {
                    self.toggle(&mut io);
                }
            }
            0x48 | 0x50 => {
                // Up and down: move the selection (and keep it in sight).
                if self.list.is_empty() {
                    return false;
                }
                let k = self.selected.and_then(|s| self.list.iter().position(|&x| x == s));
                let k = match (k, sc) {
                    (None, _) => 0,
                    (Some(k), 0x48) => k.saturating_sub(1),
                    (Some(k), _) => (k + 1).min(self.list.len() - 1),
                };
                self.selected = Some(self.list[k]);
                let c = self.content(self.body);
                let y = k as i32 * ROW_H;
                if y < self.scroll {
                    self.scroll = y;
                } else if y + ROW_H > self.scroll + c.h {
                    self.scroll = y + ROW_H - c.h;
                }
            }
            0x4B => {
                let pos = self.playing.as_ref().map(|p| p.pos).unwrap_or(0);
                self.seek(pos.saturating_sub(10_000), &mut io);
            }
            0x4D => {
                let pos = self.playing.as_ref().map(|p| p.pos).unwrap_or(0);
                self.seek(pos + 10_000, &mut io);
            }
            0x01 if matches!(self.view, View::Album(_)) => self.set_view(View::Albums),
            0x01 if matches!(self.view, View::Artist(_)) => self.set_view(View::Artists),
            _ => return false,
        }
        true
    }
}

/// A music note (two joined notes) centred at (cx, cy), for a box `size` across.
pub fn note(c: &mut Canvas, cx: i32, cy: i32, size: i32) {
    let s = size as f32 / 64.0;
    let p = |v: f32| (v * s) as i32;
    let (l, rr) = (cx - p(12.0), cx + p(12.0));
    c.circle(l - p(4.0), cy + p(12.0), p(7.0).max(2), WHITE);
    c.circle(rr - p(4.0), cy + p(8.0), p(7.0).max(2), WHITE);
    c.fill_rect(l + p(1.0), cy - p(14.0), p(3.5).max(1), p(26.0), WHITE);
    c.fill_rect(rr + p(1.0), cy - p(18.0), p(3.5).max(1), p(26.0), WHITE);
    c.line_thick(l + p(2.0), cy - p(13.0), rr + p(2.0), cy - p(17.0), p(6.0).max(2), WHITE);
}
