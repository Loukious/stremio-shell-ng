use crate::stremio_app::app::{LobbyRole, LOBBY_ROLE};
use crate::stremio_app::ipc::RPCResponse;
use crate::stremio_app::stremio_player::player::{
    CURRENT_CHAPTER, CURRENT_CHAPTER_COUNT, CURRENT_CHAPTER_TITLE, CURRENT_TIME, IS_FILE_LOADED,
    IS_PAUSED, PLAYER_CMD_TX, TOTAL_DURATION,
};
use crate::stremio_app::stremio_wevbiew::wevbiew::{CURRENT_URL, WEB_CMD_TX};
use anyhow::Context;
use reqwest::blocking::Client;
use reqwest::StatusCode;
use serde::Deserialize;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::collections::HashSet;
use std::time::{Duration, Instant};
use urlencoding::decode;

const END_OF_FILE_THRESHOLD_SECS: f64 = 0.5;

#[derive(Debug, Clone, Default)]
pub struct IntroSkipConfig {
    /// Master toggle for auto-skipping segments.
    pub enabled: bool,

    /// Segment toggles.
    pub skip_intro: bool,
    pub skip_recap: bool,
    pub skip_outro: bool,

    /// Chapter-title keyword lists (case-insensitive substring matches).
    /// Loaded from `[AutoSkipChapters] intro/recap/outro` in `RPCconfig.ini`.
    pub chapter_intro_words: Vec<String>,
    pub chapter_recap_words: Vec<String>,
    pub chapter_outro_words: Vec<String>,

    /// Optional IntroDB (introdb.app) API key.
    /// Reads do not require this, but we send it if provided.
    pub introdb_api_key: Option<String>,

    /// Optional TheIntroDB (api.theintrodb.org) API key.
    /// When provided, it can increase daily usage limits and include the requester's pending
    /// submissions in the weighted average response.
    pub theintrodb_api_key: Option<String>,
}

#[derive(Debug, Clone)]
struct IntroSegment {
    start_sec: f64,
    end_sec: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SegmentKind {
    Intro,
    Recap,
    Outro,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum SkipAction {
    Seek(f64),
    NextTrack,
}

impl SegmentKind {
    fn label(&self) -> &'static str {
        match self {
            SegmentKind::Intro => "intro",
            SegmentKind::Recap => "recap",
            SegmentKind::Outro => "outro",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IntroApi {
    IntroDbApp,
    TheIntroDb,
}

impl IntroApi {
    fn label(&self) -> &'static str {
        match self {
            IntroApi::IntroDbApp => "introdb.app",
            IntroApi::TheIntroDb => "api.theintrodb.org",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MediaKey {
    imdb_id: String,
    season: Option<u32>,
    episode: Option<u32>,
}

#[derive(Debug, Clone)]
enum ParsedVideoId {
    ImdbMovie {
        imdb_id: String,
    },
    ImdbEpisode {
        imdb_id: String,
        season: u32,
        episode: u32,
    },
    KitsuMovie {
        kitsu_id: String,
    },
    KitsuEpisode {
        kitsu_series_id: String,
        kitsu_episode: u32,
    },
}

// IntroDB (introdb.app)

#[derive(Deserialize)]
struct IntroDbSegmentsResponse {
    intro: Option<IntroDbSegment>,
    recap: Option<IntroDbSegment>,
    outro: Option<IntroDbSegment>,
}

#[derive(Deserialize)]
struct IntroDbSegment {
    start_sec: Option<f64>,
    end_sec: Option<f64>,
}

// TheIntroDB (api.theintrodb.org)

#[derive(Deserialize)]
struct TheIntroDbMediaResponse {
    #[serde(default)]
    intro: Vec<TheIntroDbTimeRange>,
    #[serde(default)]
    recap: Vec<TheIntroDbTimeRange>,
    #[serde(default)]
    credits: Vec<TheIntroDbTimeRange>,
}

#[derive(Deserialize)]
struct TheIntroDbTimeRange {
    start_ms: Option<i64>,
    end_ms: Option<i64>,
}

// Kitsu meta (anime-kitsu.strem.fun)

#[derive(Deserialize)]
struct KitsuMetaResponse {
    meta: KitsuMeta,
}

#[derive(Deserialize)]
struct KitsuMeta {
    #[serde(default)]
    imdb_id: Option<String>,
    #[serde(default)]
    videos: Vec<KitsuVideo>,
}

#[derive(Deserialize)]
struct KitsuVideo {
    id: String,
    #[serde(default)]
    imdb_id: Option<String>,
    #[serde(rename = "imdbSeason")]
    #[serde(default)]
    imdb_season: Option<u32>,
    #[serde(rename = "imdbEpisode")]
    #[serde(default)]
    imdb_episode: Option<u32>,
}

pub fn spawn_intro_skip_loop(config: IntroSkipConfig) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || run_intro_skip_loop(config))
}

fn run_intro_skip_loop(config: IntroSkipConfig) {
    // Keep this thread lightweight and resilient; never panic.
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(45))
        .http1_only()
        .user_agent(format!(
            "stremio-shell-ng/{} (intro-skip)",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .unwrap_or_else(|_| Client::new());

    let mut last_video_id: Option<String> = None;
    let mut current_media: Option<MediaKey> = None;
    let mut current_segments: Option<Vec<SkipSegment>> = None;
    let mut skipped_segments: HashSet<SegmentKey> = HashSet::new();

    let mut last_skipped_chapter: Option<i64> = None;

    let mut next_resolve_at = Instant::now();

    // Session caches
    let mut segments_cache: HashMap<MediaKey, Option<Vec<SkipSegment>>> = HashMap::new();
    let mut kitsu_cache: HashMap<String, Option<MediaKey>> = HashMap::new();

    loop {
        std::thread::sleep(Duration::from_millis(250));

        if !config.enabled {
            continue;
        }

        // Don’t fight watch-party sync as a guest.
        let role = LOBBY_ROLE.lock().map(|r| *r).unwrap_or(LobbyRole::None);
        if role == LobbyRole::Guest {
            continue;
        }

        let cur_url = CURRENT_URL.lock().map(|s| s.clone()).unwrap_or_default();
        let maybe_video_id = extract_video_id_from_url(&cur_url);

        if maybe_video_id != last_video_id {
            last_video_id = maybe_video_id.clone();
            current_media = None;
            current_segments = None;
            skipped_segments.clear();
            last_skipped_chapter = None;
            next_resolve_at = Instant::now();
        }

        // Not in player -> nothing to do.
        let Some(video_id) = maybe_video_id.as_ref() else {
            continue;
        };

        // Resolve media + fetch segments (with light retry).
        if (current_media.is_none() || current_segments.is_none())
            && Instant::now() >= next_resolve_at
        {
            match resolve_media_and_segments(
                &client,
                &config,
                video_id,
                &mut kitsu_cache,
                &mut segments_cache,
            ) {
                Ok((media, segments)) => {
                    current_media = media;
                    current_segments = segments;
                    // No more retries needed unless we change media.
                    next_resolve_at = Instant::now() + Duration::from_secs(60 * 60);
                }
                Err(e) => {
                    eprintln!("Intro skip: resolve failed: {}", format_error_chain(&e));
                    // Back off a bit before retrying.
                    next_resolve_at = Instant::now() + Duration::from_secs(15);
                }
            }
        }

        let Some(segments) = current_segments.as_ref() else {
            continue;
        };

        if segments.is_empty() {
            continue;
        }

        let (time_pos, paused, file_loaded) = (
            CURRENT_TIME.lock().map(|t| *t).unwrap_or(0.0),
            IS_PAUSED.lock().map(|p| *p).unwrap_or(false),
            IS_FILE_LOADED.lock().map(|v| *v).unwrap_or(false),
        );

        let (chapter_idx, chapter_count, chapter_title) = (
            CURRENT_CHAPTER.lock().map(|v| *v).unwrap_or(-1),
            CURRENT_CHAPTER_COUNT.lock().map(|v| *v).unwrap_or(0),
            CURRENT_CHAPTER_TITLE
                .lock()
                .map(|s| s.clone())
                .unwrap_or_default(),
        );

        if paused || !file_loaded {
            continue;
        }

        // --- Chapter-based skipping ---
        // Some files include named chapters like "Opening", "Logo", "Credits".
        // When detected, jump to the next chapter.
        if chapter_idx >= 0
            && Some(chapter_idx) != last_skipped_chapter
            && !chapter_title.trim().is_empty()
        {
            let t = chapter_title.trim().to_lowercase();
            if let Some((kind, matched)) = segment_kind_from_chapter_title(&t, &config) {
                let is_final_outro =
                    kind == SegmentKind::Outro && is_final_chapter(chapter_idx, chapter_count);
                let skip_sent = if is_final_outro {
                    send_next_track()
                } else {
                    send_add_chapter(1)
                };

                if skip_sent {
                    last_skipped_chapter = Some(chapter_idx);
                    if is_final_outro {
                        println!(
                            "⏭️ AutoSkip final chapter {}: '{}' (chapter #{}/{}, matched '{}') -> next track",
                            kind.label(),
                            chapter_title,
                            chapter_idx,
                            chapter_count,
                            matched
                        );
                    } else {
                        println!(
                            "⏭️ AutoSkip chapter {}: '{}' (chapter #{}, matched '{}')",
                            kind.label(),
                            chapter_title,
                            chapter_idx,
                            matched
                        );
                    }
                    continue;
                }
            }
        }

        // --- Segment-based skipping (IntroDB / TheIntroDB) ---
        let Some(segments) = current_segments.as_ref() else {
            continue;
        };

        if segments.is_empty() {
            continue;
        }

        // Guard: only skip when we are inside a segment.
        // Use a small epsilon so we still skip if playback starts slightly after start_sec.
        let epsilon = 1.0_f64;

        if let Some(seg) = segments.iter().find(|seg| {
            if skipped_segments.contains(&seg.key()) {
                return false;
            }
            if !config.segment_enabled(seg.kind) {
                return false;
            }

            if !is_plausible_outro(seg) {
                return false;
            }

            let start = seg.segment.start_sec;
            let end = seg.segment.end_sec.unwrap_or(f64::INFINITY);
            time_pos + epsilon >= start && time_pos < end
        }) {
            let action = effective_skip_action(seg.kind, seg.segment.end_sec);
            if let Some(action) = action {
                let action_sent = match action {
                    SkipAction::Seek(target_sec) if target_sec <= time_pos + 0.05 => {
                        skipped_segments.insert(seg.key());
                        continue;
                    }
                    SkipAction::Seek(target_sec) => send_seek_absolute(target_sec),
                    SkipAction::NextTrack => send_next_track(),
                };

                if action_sent {
                    skipped_segments.insert(seg.key());

                    let media_text = current_media
                        .as_ref()
                        .map(format_media_key)
                        .unwrap_or_else(|| "<unknown media>".to_string());
                    match action {
                        SkipAction::Seek(target_sec) => println!(
                            "⏭️ AutoSkip {} ({}) {} -> {:.3}s (segment {:.3}-{})",
                            seg.kind.label(),
                            seg.api.label(),
                            media_text,
                            target_sec,
                            seg.segment.start_sec,
                            seg.segment
                                .end_sec
                                .map(|v| format!("{v:.3}"))
                                .unwrap_or_else(|| "end".to_string())
                        ),
                        SkipAction::NextTrack => println!(
                            "⏭️ AutoSkip {} ({}) {} -> next track (segment {:.3}-{})",
                            seg.kind.label(),
                            seg.api.label(),
                            media_text,
                            seg.segment.start_sec,
                            seg.segment
                                .end_sec
                                .map(|v| format!("{v:.3}"))
                                .unwrap_or_else(|| "end".to_string())
                        ),
                    }
                }
            } else {
                // End is unknown and duration isn't available yet.
            }
        }
    }
}

#[derive(Debug, Clone)]
struct SkipSegment {
    api: IntroApi,
    kind: SegmentKind,
    segment: IntroSegment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SegmentKey {
    kind: SegmentKind,
    start_ms: i64,
    end_ms: i64,
}

impl SkipSegment {
    fn key(&self) -> SegmentKey {
        SegmentKey {
            kind: self.kind,
            start_ms: (self.segment.start_sec * 1000.0).round() as i64,
            end_ms: self
                .segment
                .end_sec
                .map(|v| (v * 1000.0).round() as i64)
                .unwrap_or(-1),
        }
    }
}

impl IntroSkipConfig {
    fn segment_enabled(&self, kind: SegmentKind) -> bool {
        match kind {
            SegmentKind::Intro => self.skip_intro,
            SegmentKind::Recap => self.skip_recap,
            SegmentKind::Outro => self.skip_outro,
        }
    }
}

fn resolve_media_and_segments(
    client: &Client,
    config: &IntroSkipConfig,
    video_id: &str,
    kitsu_cache: &mut HashMap<String, Option<MediaKey>>,
    segments_cache: &mut HashMap<MediaKey, Option<Vec<SkipSegment>>>,
) -> anyhow::Result<(Option<MediaKey>, Option<Vec<SkipSegment>>)> {
    let parsed = match parse_video_id(video_id) {
        Some(p) => p,
        None => return Ok((None, None)),
    };

    let media = match parsed {
        ParsedVideoId::ImdbMovie { imdb_id } => {
            if !looks_like_imdb_id(&imdb_id) {
                None
            } else {
                Some(MediaKey {
                    imdb_id,
                    season: None,
                    episode: None,
                })
            }
        }
        ParsedVideoId::ImdbEpisode {
            imdb_id,
            season,
            episode,
        } => {
            if !looks_like_imdb_id(&imdb_id) || season == 0 || episode == 0 {
                None
            } else {
                Some(MediaKey {
                    imdb_id,
                    season: Some(season),
                    episode: Some(episode),
                })
            }
        }
        ParsedVideoId::KitsuEpisode {
            kitsu_series_id,
            kitsu_episode,
        } => {
            let kitsu_video_id = format!("kitsu:{kitsu_series_id}:{kitsu_episode}");
            if let Some(cached) = kitsu_cache.get(&kitsu_video_id) {
                cached.clone()
            } else {
                resolve_kitsu_episode_to_imdb_media(
                    client,
                    &kitsu_series_id,
                    &kitsu_video_id,
                    kitsu_cache,
                )?
            }
        }
        ParsedVideoId::KitsuMovie { kitsu_id } => {
            let kitsu_video_id = format!("kitsu:{kitsu_id}");
            if let Some(cached) = kitsu_cache.get(&kitsu_video_id) {
                cached.clone()
            } else {
                resolve_kitsu_movie_to_imdb_media(client, &kitsu_id, &kitsu_video_id, kitsu_cache)?
            }
        }
    };

    let Some(media_key) = media else {
        return Ok((None, None));
    };

    // Fetch segments (cached).
    let segments = if let Some(cached) = segments_cache.get(&media_key) {
        cached.clone()
    } else {
        let fetched = fetch_skip_segments(client, config, &media_key)?;
        segments_cache.insert(media_key.clone(), fetched.clone());
        fetched
    };

    Ok((Some(media_key), segments))
}

fn fetch_skip_segments(
    client: &Client,
    config: &IntroSkipConfig,
    key: &MediaKey,
) -> anyhow::Result<Option<Vec<SkipSegment>>> {
    let want_intro = config.skip_intro;
    let want_recap = config.skip_recap;
    let want_outro = config.skip_outro;

    let mut segments: Vec<SkipSegment> = Vec::new();
    let mut have_intro = false;
    let mut have_recap = false;
    let mut have_outro = false;
    let mut introdb_error: Option<anyhow::Error> = None;

    // Prefer introdb.app for TV episodes (fast + precise).
    if key.season.is_some() && key.episode.is_some() {
        match fetch_introdb_app_segments(client, config, key) {
            Ok(introdb_segments) => {
                println!(
                    "Intro skip: introdb.app returned {} usable segment(s) for {}",
                    introdb_segments.len(),
                    format_media_key(key)
                );
                for seg in introdb_segments {
                    match seg.kind {
                        SegmentKind::Intro => have_intro = true,
                        SegmentKind::Recap => have_recap = true,
                        SegmentKind::Outro => have_outro = true,
                    }
                    segments.push(seg);
                }
            }
            Err(e) => {
                // Soft-fail; still try TheIntroDB for coverage.
                eprintln!(
                    "Intro skip: introdb.app fetch failed: {}",
                    format_error_chain(&e)
                );
                introdb_error = Some(e);
            }
        }
    }

    let missing_intro = want_intro && !have_intro;
    let missing_recap = want_recap && !have_recap;
    let missing_outro = want_outro && !have_outro;

    // TheIntroDB supports movies and TV episodes via imdb_id and can fill gaps.
    if key.season.is_none()
        || key.episode.is_none()
        || missing_intro
        || missing_recap
        || missing_outro
    {
        match fetch_theintrodb_segments(client, config, key) {
            Ok(theintrodb_segments) => {
                println!(
                    "Intro skip: api.theintrodb.org returned {} usable segment(s) for {}",
                    theintrodb_segments.len(),
                    format_media_key(key)
                );
                for seg in theintrodb_segments {
                    // Only fill missing kinds for TV episodes; for movies, just take what exists.
                    let allow = if key.season.is_some() && key.episode.is_some() {
                        match seg.kind {
                            SegmentKind::Intro => missing_intro,
                            SegmentKind::Recap => missing_recap,
                            SegmentKind::Outro => missing_outro,
                        }
                    } else {
                        true
                    };

                    if allow {
                        segments.push(seg);
                    }
                }
            }
            Err(e) => {
                // If introdb.app already provided something, still return it.
                if segments.is_empty() {
                    return Err(e);
                }
            }
        }
    }

    // Filter out disabled kinds (if any) to keep matching simple.
    segments.retain(|s| config.segment_enabled(s.kind));

    if segments.is_empty() {
        if let Some(err) = introdb_error {
            // introdb.app failed and we didn't obtain any fallback segments; retry soon.
            return Err(err);
        }
        return Ok(None);
    }

    segments.sort_by(|a, b| {
        a.segment
            .start_sec
            .partial_cmp(&b.segment.start_sec)
            .unwrap_or(Ordering::Equal)
    });

    Ok(Some(segments))
}

fn fetch_introdb_app_segments(
    client: &Client,
    config: &IntroSkipConfig,
    key: &MediaKey,
) -> anyhow::Result<Vec<SkipSegment>> {
    let (Some(season), Some(episode)) = (key.season, key.episode) else {
        return Ok(Vec::new());
    };

    let url = format!(
        "https://api.introdb.app/segments?imdb_id={}&season={}&episode={}",
        key.imdb_id, season, episode
    );

    let mut req = client.get(&url);
    if let Some(api_key) = config
        .introdb_api_key
        .as_ref()
        .filter(|s| !s.trim().is_empty())
    {
        req = req.header("X-API-Key", api_key.trim());
    }

    let resp = req
        .send()
        .with_context(|| format!("request failed for {url}"))?;

    if resp.status() == StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }

    let resp = resp
        .error_for_status()
        .with_context(|| format!("non-success response from {url}"))?;
    let parsed: IntroDbSegmentsResponse = resp
        .json()
        .with_context(|| format!("invalid JSON response from {url}"))?;

    let mut out: Vec<SkipSegment> = Vec::new();
    if config.skip_intro {
        if let Some(seg) = normalize_introdb_segment(parsed.intro, false) {
            out.push(SkipSegment {
                api: IntroApi::IntroDbApp,
                kind: SegmentKind::Intro,
                segment: seg,
            });
        }
    }
    if config.skip_recap {
        if let Some(seg) = normalize_introdb_segment(parsed.recap, false) {
            out.push(SkipSegment {
                api: IntroApi::IntroDbApp,
                kind: SegmentKind::Recap,
                segment: seg,
            });
        }
    }
    if config.skip_outro {
        if let Some(seg) = normalize_introdb_segment(parsed.outro, true) {
            out.push(SkipSegment {
                api: IntroApi::IntroDbApp,
                kind: SegmentKind::Outro,
                segment: seg,
            });
        }
    }

    Ok(out)
}

fn format_error_chain(error: &anyhow::Error) -> String {
    error
        .chain()
        .map(|cause| cause.to_string())
        .collect::<Vec<_>>()
        .join(": ")
}

fn normalize_introdb_segment(
    seg: Option<IntroDbSegment>,
    allow_missing_end: bool,
) -> Option<IntroSegment> {
    let seg = seg?;
    let start_sec = seg.start_sec.unwrap_or(0.0);
    let end_sec = seg.end_sec;

    if !start_sec.is_finite() {
        return None;
    }

    if let Some(end) = end_sec {
        if !end.is_finite() || end <= start_sec {
            return None;
        }
        return Some(IntroSegment {
            start_sec,
            end_sec: Some(end),
        });
    }

    if allow_missing_end {
        return Some(IntroSegment {
            start_sec,
            end_sec: None,
        });
    }

    None
}

fn fetch_theintrodb_segments(
    client: &Client,
    config: &IntroSkipConfig,
    key: &MediaKey,
) -> anyhow::Result<Vec<SkipSegment>> {
    let mut url = format!(
        "https://api.theintrodb.org/v2/media?imdb_id={}",
        key.imdb_id
    );

    if let (Some(season), Some(episode)) = (key.season, key.episode) {
        url.push_str(&format!("&season={season}&episode={episode}"));
    }

    let mut req = client.get(&url);
    if let Some(api_key) = config
        .theintrodb_api_key
        .as_ref()
        .filter(|s| !s.trim().is_empty())
    {
        req = req.header("Authorization", format!("Bearer {}", api_key.trim()));
    }

    let resp = req
        .send()
        .with_context(|| format!("request failed for {url}"))?;

    if resp.status() == StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }

    let resp = resp
        .error_for_status()
        .with_context(|| format!("non-success response from {url}"))?;
    let parsed: TheIntroDbMediaResponse = resp
        .json()
        .with_context(|| format!("invalid JSON response from {url}"))?;

    let mut out: Vec<SkipSegment> = Vec::new();

    if config.skip_intro {
        out.extend(parse_theintrodb_ranges(
            parsed.intro,
            SegmentKind::Intro,
            false,
        ));
    }
    if config.skip_recap {
        out.extend(parse_theintrodb_ranges(
            parsed.recap,
            SegmentKind::Recap,
            false,
        ));
    }
    if config.skip_outro {
        // TheIntroDB models "outro" as credits.
        out.extend(parse_theintrodb_ranges(
            parsed.credits,
            SegmentKind::Outro,
            true,
        ));
    }

    Ok(out)
}

fn parse_theintrodb_ranges(
    ranges: Vec<TheIntroDbTimeRange>,
    kind: SegmentKind,
    allow_missing_end: bool,
) -> Vec<SkipSegment> {
    let mut out: Vec<SkipSegment> = ranges
        .into_iter()
        .filter_map(|seg| {
            let start_ms = seg.start_ms.unwrap_or(0);
            let end_ms = seg.end_ms;

            if let Some(end_ms) = end_ms {
                if end_ms <= start_ms {
                    return None;
                }
            } else if !allow_missing_end {
                return None;
            }

            let start_sec = (start_ms as f64) / 1000.0;
            let end_sec = end_ms.map(|v| (v as f64) / 1000.0);

            if !start_sec.is_finite() {
                return None;
            }
            if let Some(end_sec) = end_sec {
                if !end_sec.is_finite() || end_sec <= start_sec {
                    return None;
                }
            }

            Some(SkipSegment {
                api: IntroApi::TheIntroDb,
                kind,
                segment: IntroSegment { start_sec, end_sec },
            })
        })
        .collect();

    out.sort_by(|a, b| {
        a.segment
            .start_sec
            .partial_cmp(&b.segment.start_sec)
            .unwrap_or(Ordering::Equal)
    });

    out
}

fn effective_skip_action(kind: SegmentKind, end_sec: Option<f64>) -> Option<SkipAction> {
    let duration = TOTAL_DURATION.lock().map(|d| *d).unwrap_or(0.0);
    effective_skip_action_for_duration(kind, end_sec, duration)
}

fn effective_skip_action_for_duration(
    kind: SegmentKind,
    end_sec: Option<f64>,
    duration: f64,
) -> Option<SkipAction> {
    if kind == SegmentKind::Outro && end_sec.is_none() {
        return Some(SkipAction::NextTrack);
    }

    let end_sec = end_sec.filter(|end| end.is_finite() && *end >= 0.0)?;
    let has_duration = duration.is_finite() && duration > END_OF_FILE_THRESHOLD_SECS;
    if has_duration && end_sec > duration {
        return (kind == SegmentKind::Outro).then_some(SkipAction::NextTrack);
    }
    if kind == SegmentKind::Outro
        && has_duration
        && end_sec >= duration - END_OF_FILE_THRESHOLD_SECS
    {
        return Some(SkipAction::NextTrack);
    }

    Some(SkipAction::Seek(end_sec))
}

fn is_final_chapter(chapter_idx: i64, chapter_count: i64) -> bool {
    chapter_count > 0 && chapter_idx == chapter_count - 1
}

fn is_plausible_outro(seg: &SkipSegment) -> bool {
    if seg.kind != SegmentKind::Outro {
        return true;
    }

    // TheIntroDB uses a "credits" pool which can include early credit sequences.
    // To avoid skipping the wrong content, only treat it as an outro when it starts
    // in the latter half of the media.
    if seg.api != IntroApi::TheIntroDb {
        return true;
    }

    let duration = TOTAL_DURATION.lock().map(|d| *d).unwrap_or(0.0);
    if !duration.is_finite() || duration <= 0.0 {
        return false;
    }

    seg.segment.start_sec >= duration * 0.5
}

fn format_media_key(key: &MediaKey) -> String {
    match (key.season, key.episode) {
        (Some(season), Some(episode)) => format!("{} S{}E{}", key.imdb_id, season, episode),
        _ => key.imdb_id.clone(),
    }
}

fn resolve_kitsu_episode_to_imdb_media(
    client: &Client,
    kitsu_series_id: &str,
    target_kitsu_video_id: &str,
    kitsu_cache: &mut HashMap<String, Option<MediaKey>>,
) -> anyhow::Result<Option<MediaKey>> {
    let url = format!("https://anime-kitsu.strem.fun/meta/series/kitsu:{kitsu_series_id}.json");

    let resp = client.get(url).send()?;
    if resp.status() == StatusCode::NOT_FOUND {
        kitsu_cache.insert(target_kitsu_video_id.to_string(), None);
        return Ok(None);
    }
    let resp = resp.error_for_status()?;

    let meta: KitsuMetaResponse = resp.json()?;

    // Populate cache for all videos we can map from this response.
    for v in &meta.meta.videos {
        let imdb_id = v.imdb_id.clone().or_else(|| meta.meta.imdb_id.clone());
        let (Some(imdb_id), Some(season), Some(episode)) = (imdb_id, v.imdb_season, v.imdb_episode)
        else {
            continue;
        };

        if !looks_like_imdb_id(&imdb_id) || season == 0 || episode == 0 {
            continue;
        }

        kitsu_cache.insert(
            v.id.clone(),
            Some(MediaKey {
                imdb_id,
                season: Some(season),
                episode: Some(episode),
            }),
        );
    }

    Ok(kitsu_cache
        .get(target_kitsu_video_id)
        .cloned()
        .unwrap_or(None))
}

fn resolve_kitsu_movie_to_imdb_media(
    client: &Client,
    kitsu_id: &str,
    target_kitsu_video_id: &str,
    kitsu_cache: &mut HashMap<String, Option<MediaKey>>,
) -> anyhow::Result<Option<MediaKey>> {
    let url = format!("https://anime-kitsu.strem.fun/meta/movie/kitsu:{kitsu_id}.json");

    let resp = client.get(url).send()?;
    if resp.status() == StatusCode::NOT_FOUND {
        kitsu_cache.insert(target_kitsu_video_id.to_string(), None);
        return Ok(None);
    }
    let resp = resp.error_for_status()?;

    let meta: KitsuMetaResponse = resp.json()?;

    let imdb_id = meta.meta.imdb_id.unwrap_or_default();
    let media = if looks_like_imdb_id(&imdb_id) {
        Some(MediaKey {
            imdb_id,
            season: None,
            episode: None,
        })
    } else {
        None
    };

    kitsu_cache.insert(target_kitsu_video_id.to_string(), media.clone());
    Ok(media)
}

fn send_seek_absolute(target_sec: f64) -> bool {
    let cmd = format!(r#"["mpv-command",["seek","{}","absolute"]]"#, target_sec);

    if let Ok(guard) = PLAYER_CMD_TX.lock() {
        if let Some(tx) = guard.as_ref() {
            return tx.send(cmd).is_ok();
        }
    }

    false
}

fn send_add_chapter(delta: i64) -> bool {
    let cmd = format!(r#"["mpv-command",["add","chapter","{}"]]"#, delta);

    if let Ok(guard) = PLAYER_CMD_TX.lock() {
        if let Some(tx) = guard.as_ref() {
            return tx.send(cmd).is_ok();
        }
    }

    false
}

fn send_next_track() -> bool {
    if let Ok(guard) = WEB_CMD_TX.lock() {
        if let Some(tx) = guard.as_ref() {
            return tx.send(RPCResponse::media_key("next-track")).is_ok();
        }
    }

    false
}

fn segment_kind_from_chapter_title<'a>(
    title_lower: &str,
    config: &'a IntroSkipConfig,
) -> Option<(SegmentKind, &'a str)> {
    if config.skip_intro {
        if let Some(matched) = find_matching_keyword(title_lower, &config.chapter_intro_words) {
            return Some((SegmentKind::Intro, matched));
        }
    }

    if config.skip_recap {
        if let Some(matched) = find_matching_keyword(title_lower, &config.chapter_recap_words) {
            return Some((SegmentKind::Recap, matched));
        }
    }

    if config.skip_outro {
        if let Some(matched) = find_matching_keyword(title_lower, &config.chapter_outro_words) {
            return Some((SegmentKind::Outro, matched));
        }
    }

    None
}

fn find_matching_keyword<'a>(
    haystack_lower: &str,
    keywords_lower: &'a [String],
) -> Option<&'a str> {
    keywords_lower
        .iter()
        .find(|keyword| chapter_keyword_matches(haystack_lower, keyword))
        .map(|k| k.as_str())
}

fn chapter_keyword_matches(haystack_lower: &str, keyword: &str) -> bool {
    if keyword.is_empty() {
        return false;
    }

    if keyword.len() <= 3 && keyword.chars().all(|c| c.is_ascii_alphanumeric()) {
        return haystack_lower
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|token| token == keyword);
    }

    haystack_lower.contains(keyword)
}

fn extract_video_id_from_url(cur_url: &str) -> Option<String> {
    if !cur_url.contains("/player/") {
        return None;
    }

    let cur_url = cur_url.trim_end_matches('/');
    let last = cur_url.split('/').next_back().unwrap_or("");
    let last = last.split('?').next().unwrap_or(last);
    let decoded = decode(last).ok()?;
    let decoded = decoded.as_ref();

    if decoded.is_empty() {
        None
    } else {
        Some(decoded.to_string())
    }
}

fn parse_video_id(video_id: &str) -> Option<ParsedVideoId> {
    let parts: Vec<&str> = video_id.split(':').collect();
    if parts.is_empty() {
        return None;
    }

    if parts[0] == "kitsu" {
        return match parts.len() {
            2 => Some(ParsedVideoId::KitsuMovie {
                kitsu_id: parts[1].to_string(),
            }),
            3 => Some(ParsedVideoId::KitsuEpisode {
                kitsu_series_id: parts[1].to_string(),
                kitsu_episode: parts[2].parse::<u32>().ok()?,
            }),
            _ => None,
        };
    }

    match parts.len() {
        1 => {
            let imdb_id = parts[0].to_string();
            if looks_like_imdb_id(&imdb_id) {
                Some(ParsedVideoId::ImdbMovie { imdb_id })
            } else {
                None
            }
        }
        3 => Some(ParsedVideoId::ImdbEpisode {
            imdb_id: parts[0].to_string(),
            season: parts[1].parse::<u32>().ok()?,
            episode: parts[2].parse::<u32>().ok()?,
        }),
        _ => None,
    }
}

fn looks_like_imdb_id(id: &str) -> bool {
    let id = id.trim();
    if !id.starts_with("tt") {
        return false;
    }
    let digits = &id[2..];
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::{
        chapter_keyword_matches, effective_skip_action_for_duration, is_final_chapter, SegmentKind,
        SkipAction,
    };

    #[test]
    fn terminal_outro_advances_without_seeking_to_media_tail() {
        assert_eq!(
            effective_skip_action_for_duration(SegmentKind::Outro, Some(2_835.0), 2_832.121),
            Some(SkipAction::NextTrack)
        );
        assert_eq!(
            effective_skip_action_for_duration(SegmentKind::Outro, None, 0.0),
            Some(SkipAction::NextTrack)
        );
    }

    #[test]
    fn valid_segment_end_is_preserved() {
        assert_eq!(
            effective_skip_action_for_duration(SegmentKind::Intro, Some(254.0), 2_832.121),
            Some(SkipAction::Seek(254.0))
        );
    }

    #[test]
    fn nonterminal_outro_still_seeks_to_its_explicit_end() {
        assert_eq!(
            effective_skip_action_for_duration(SegmentKind::Outro, Some(90.0), 100.0),
            Some(SkipAction::Seek(90.0))
        );
    }

    #[test]
    fn invalid_non_outro_end_is_rejected() {
        assert_eq!(
            effective_skip_action_for_duration(SegmentKind::Intro, Some(f64::NAN), 0.0),
            None
        );
        assert_eq!(
            effective_skip_action_for_duration(SegmentKind::Recap, None, 100.0),
            None
        );
        assert_eq!(
            effective_skip_action_for_duration(SegmentKind::Intro, Some(101.0), 100.0),
            None
        );
    }

    #[test]
    fn final_chapter_detection_uses_zero_based_index() {
        assert!(is_final_chapter(3, 4));
        assert!(!is_final_chapter(2, 4));
        assert!(!is_final_chapter(-1, 0));
    }

    #[test]
    fn short_chapter_keywords_match_whole_tokens_only() {
        assert!(chapter_keyword_matches("op", "op"));
        assert!(chapter_keyword_matches("op - opening theme", "op"));
        assert!(!chapter_keyword_matches("hope", "op"));
        assert!(chapter_keyword_matches("ed 1", "ed"));
        assert!(!chapter_keyword_matches("credits", "ed"));
    }
}
