//! The `.kantan` file: one self-contained binary file holding the audio, the
//! clips and the saved settings. Undo history is not kept.

use std::collections::BTreeMap;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::project::{Clip, ClipId, Project, Segment, SegmentKind, Source, SourceId};

pub const EXTENSION: &str = "kantan";
const MAGIC: &[u8; 8] = b"KANTANP1";

const SILENCE: u8 = 0;
const AUDIO: u8 = 1;

/// Name and value pairs stored alongside the project.
pub type Settings = Vec<(String, String)>;

/// Writes the project, replacing `path` only once the file is complete.
pub fn save(path: &Path, project: &Project, settings: &Settings) -> Result<(), String> {
    let mut temp = path.as_os_str().to_owned();
    temp.push(".part");
    let temp = PathBuf::from(temp);
    let result = write_file(&temp, project, settings)
        .and_then(|()| std::fs::rename(&temp, path))
        .map_err(|e| e.to_string());
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn write_file(path: &Path, project: &Project, settings: &Settings) -> std::io::Result<()> {
    let mut out = BufWriter::new(std::fs::File::create(path)?);
    write_to(&mut out, project, settings)
}

/// The project as file contents, for the browser version to download.
pub fn to_bytes(project: &Project, settings: &Settings) -> Vec<u8> {
    let mut out = Vec::new();
    write_to(&mut out, project, settings).expect("writing to memory can't fail");
    out
}

/// Reads a project from file contents, as uploaded in the browser version.
pub fn load_bytes(bytes: &[u8]) -> Result<(Project, Settings), String> {
    parse(bytes).ok_or_else(|| "not a Kantan project, or the file is damaged".to_owned())
}

fn write_to(mut out: &mut impl Write, project: &Project, settings: &Settings) -> std::io::Result<()> {
    out.write_all(MAGIC)?;
    let (next_source, next_clip) = project.next_ids();
    write_u32(&mut out, project.sample_rate())?;
    write_u32(&mut out, next_source)?;
    write_u32(&mut out, next_clip)?;

    // Audio only reachable through undo history isn't worth keeping.
    let mut used: Vec<SourceId> = project
        .clips()
        .iter()
        .flat_map(|clip| &clip.segments)
        .filter_map(|segment| match segment.kind {
            SegmentKind::Audio { source, .. } => Some(source),
            SegmentKind::Silence => None,
        })
        .collect();
    used.sort_unstable();
    used.dedup();

    write_u32(&mut out, used.len() as u32)?;
    for id in used {
        let source = project.source(id).expect("clips only use existing sources");
        write_u32(&mut out, id.0)?;
        write_u32(&mut out, source.channels as u32)?;
        write_u64(&mut out, source.samples.len() as u64)?;
        for sample in &source.samples {
            out.write_all(&sample.to_le_bytes())?;
        }
    }

    write_u32(&mut out, project.clips().len() as u32)?;
    for clip in project.clips() {
        write_u32(&mut out, clip.id.0)?;
        write_str(&mut out, &clip.name)?;
        write_u64(&mut out, clip.start)?;
        write_u32(&mut out, clip.segments.len() as u32)?;
        for segment in &clip.segments {
            match segment.kind {
                SegmentKind::Silence => out.write_all(&[SILENCE])?,
                SegmentKind::Audio { source, offset, gain } => {
                    out.write_all(&[AUDIO])?;
                    write_u32(&mut out, source.0)?;
                    write_u64(&mut out, offset)?;
                    out.write_all(&gain.to_le_bytes())?;
                }
            }
            write_u64(&mut out, segment.len)?;
        }
    }

    write_u32(&mut out, settings.len() as u32)?;
    for (name, value) in settings {
        write_str(&mut out, name)?;
        write_str(&mut out, value)?;
    }
    out.flush()
}

fn write_u32(out: &mut impl Write, value: u32) -> std::io::Result<()> {
    out.write_all(&value.to_le_bytes())
}

fn write_u64(out: &mut impl Write, value: u64) -> std::io::Result<()> {
    out.write_all(&value.to_le_bytes())
}

fn write_str(out: &mut impl Write, text: &str) -> std::io::Result<()> {
    write_u32(out, text.len() as u32)?;
    out.write_all(text.as_bytes())
}

/// Reads a project, rejecting damaged files instead of trusting their contents.
pub fn load(path: &Path) -> Result<(Project, Settings), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    parse(&bytes).ok_or_else(|| "not a Kantan project, or the file is damaged".to_owned())
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, count: usize) -> Option<&'a [u8]> {
        if count > self.0.len() {
            return None;
        }
        let (head, tail) = self.0.split_at(count);
        self.0 = tail;
        Some(head)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn string(&mut self) -> Option<String> {
        let len = self.u32()? as usize;
        String::from_utf8(self.take(len)?.to_vec()).ok()
    }
}

fn parse(bytes: &[u8]) -> Option<(Project, Settings)> {
    let mut r = Reader(bytes);
    if r.take(MAGIC.len())? != MAGIC {
        return None;
    }
    let sample_rate = r.u32()?;
    let next_source = r.u32()?;
    let next_clip = r.u32()?;
    if sample_rate == 0 {
        return None;
    }

    let mut sources = BTreeMap::new();
    for _ in 0..r.u32()? {
        let id = SourceId(r.u32()?);
        let channels = u16::try_from(r.u32()?).ok()?;
        let count = usize::try_from(r.u64()?).ok()?;
        let data = r.take(count.checked_mul(4)?)?;
        let samples = data
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        if id.0 >= next_source {
            return None;
        }
        sources.insert(id, Arc::new(Source::new(channels, samples).ok()?));
    }

    let mut clips = Vec::new();
    let mut previous_end = 0;
    for _ in 0..r.u32()? {
        let id = ClipId(r.u32()?);
        let name = r.string()?;
        let start = r.u64()?;
        let mut segments = Vec::new();
        for _ in 0..r.u32()? {
            let kind = match r.u8()? {
                SILENCE => SegmentKind::Silence,
                AUDIO => SegmentKind::Audio {
                    source: SourceId(r.u32()?),
                    offset: r.u64()?,
                    gain: r.f32()?,
                },
                _ => return None,
            };
            let len = r.u64()?;
            if len == 0 {
                return None;
            }
            if let SegmentKind::Audio { source, offset, gain } = kind {
                let frames = sources.get(&source)?.frames();
                if !gain.is_finite() || offset.checked_add(len)? > frames {
                    return None;
                }
            }
            segments.push(Segment { kind, len });
        }
        let clip = Clip { id, name, start, segments };
        // The track's rules: named, not empty, in order and never overlapping.
        if clip.name.is_empty() || clip.segments.is_empty() || id.0 >= next_clip || start < previous_end {
            return None;
        }
        previous_end = start.checked_add(clip.len())?;
        clips.push(clip);
    }

    let mut settings = Vec::new();
    for _ in 0..r.u32()? {
        settings.push((r.string()?, r.string()?));
    }
    Some((Project::from_parts(sample_rate, sources, clips, next_source, next_clip), settings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TimeRange, render_clip};

    fn sample_project() -> Project {
        let mut p = Project::new(48000);
        let a = p.add_source(1, vec![0.25; 1000]).unwrap();
        let a = p.append_clip("声 line", a).unwrap();
        let b = p.add_source(2, vec![0.5, -0.5, 0.1, -0.1]).unwrap();
        let b = p.append_clip("b", b).unwrap();
        p.amplify(TimeRange::new(10, 20), 6.0).unwrap();
        p.split_at(500).unwrap();
        let _ = (a, b);
        let c = p.add_source(1, vec![0.5; 10]).unwrap();
        let c = p.append_clip("c", c).unwrap();
        let d = p.add_source(1, vec![0.5; 10]).unwrap();
        let d = p.append_clip("d", d).unwrap();
        p.merge_clips(&[c, d]).unwrap();
        p
    }

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("kantan_file_{}_{name}", std::process::id()))
    }

    #[test]
    fn a_project_round_trips_exactly() {
        let path = temp("round.kantan");
        let project = sample_project();
        let settings = vec![("prefix".to_owned(), "VO_".to_owned())];
        save(&path, &project, &settings).unwrap();
        let (loaded, loaded_settings) = load(&path).unwrap();
        std::fs::remove_file(&path).unwrap();

        assert_eq!(loaded, project);
        assert_eq!(loaded_settings, settings);
        for clip in project.clips() {
            assert_eq!(render_clip(&loaded, clip), render_clip(&project, clip));
        }
    }

    #[test]
    fn editing_continues_with_fresh_ids_after_loading() {
        let path = temp("ids.kantan");
        save(&path, &sample_project(), &Vec::new()).unwrap();
        let (mut loaded, _) = load(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        let source = loaded.add_source(1, vec![0.1]).unwrap();
        let id = loaded.append_clip("new", source).unwrap();
        assert_eq!(loaded.clips().iter().filter(|c| c.id == id).count(), 1);
    }

    #[test]
    fn audio_only_in_undo_history_is_not_saved() {
        let path = temp("prune.kantan");
        let mut p = Project::new(100);
        let a = p.add_source(1, vec![0.5; 100_000]).unwrap();
        let a = p.append_clip("a", a).unwrap();
        p.delete_clips(&[a]).unwrap();
        save(&path, &p, &Vec::new()).unwrap();
        let size = std::fs::metadata(&path).unwrap().len();
        std::fs::remove_file(&path).unwrap();
        assert!(size < 1000, "{size}");
    }

    #[test]
    fn any_truncation_or_garbage_is_an_error_not_a_panic() {
        let path = temp("full.kantan");
        save(&path, &sample_project(), &vec![("a".to_owned(), "b".to_owned())]).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();

        for len in 0..bytes.len() {
            assert!(parse(&bytes[..len]).is_none(), "truncated at {len}");
        }
        // Damage every byte in turn. It may still parse if harmless, but must not panic.
        for index in 0..bytes.len().min(600) {
            let mut damaged = bytes.clone();
            damaged[index] ^= 0xFF;
            let _ = parse(&damaged);
        }
        assert!(parse(b"KANTANP1").is_none());
        assert!(parse(b"something else entirely").is_none());
    }

    #[test]
    fn a_failed_save_keeps_the_old_file() {
        let path = temp("missing_dir").join("a.kantan");
        assert!(save(&path, &sample_project(), &Vec::new()).is_err());
    }
}
