//! The download queue against a fake Hub: parts, waiting out a busy server,
//! pause, cancel, and continuing after a restart.
#![cfg(target_os = "macos")]

mod common;

use common::{COMMIT, PART_SIZE, REPO, Ranged, engine, git_oid, sha256};
use dedupd_core::Home;
use dedupd_core::place::App;
use dedupd_core::pull::Engine;
use dedupd_core::queue::{FileStage, JobRecord, Queue, State};
use serde_json::json;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const MODEL: &str = "org/Repo-GGUF:Q4_K_M";
const WEIGHTS: &str = "model-Q4_K_M.gguf";

struct Upstream {
    server: MockServer,
    weights: Vec<u8>,
    config: Vec<u8>,
}

impl Upstream {
    fn weights_path() -> String {
        format!("/{REPO}/resolve/{COMMIT}/{WEIGHTS}")
    }

    /// The `Range` headers of the requests for the weights, in order.
    async fn weight_ranges(&self) -> Vec<String> {
        let requests = self.server.received_requests().await.unwrap();
        requests
            .iter()
            .filter(|r| r.url.path() == Self::weights_path())
            .map(|r| {
                r.headers
                    .get("range")
                    .map(|v| v.to_str().unwrap().to_string())
                    .unwrap_or_default()
            })
            .collect()
    }
}

/// A Hub with one repository: weights of four parts and a small config.
/// With `files`, the files themselves are served too.
async fn upstream(files: bool) -> Upstream {
    let weights: Vec<u8> = (0..4 * PART_SIZE as u32).map(|i| (i % 251) as u8).collect();
    let config = b"{\"a\":1}\n".to_vec();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("/api/models/{REPO}/revision/main")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"sha": COMMIT, "id": REPO})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/models/{REPO}/revision/{COMMIT}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"sha": COMMIT, "id": REPO})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("/api/models/{REPO}/tree/{COMMIT}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            {"type": "file", "path": WEIGHTS, "size": weights.len(), "oid": "pointer",
             "lfs": {"oid": sha256(&weights), "size": weights.len(), "pointerSize": 134}},
            {"type": "file", "path": "config.json", "size": config.len(), "oid": git_oid(&config)}
        ])))
        .mount(&server)
        .await;
    let up = Upstream {
        server,
        weights,
        config,
    };
    if files {
        serve_files(&up).await;
    }
    up
}

async fn serve_files(up: &Upstream) {
    for (file, body) in [(WEIGHTS, &up.weights), ("config.json", &up.config)] {
        Mock::given(method("GET"))
            .and(path(format!("/{REPO}/resolve/{COMMIT}/{file}")))
            .respond_with(Ranged(body.clone()))
            .mount(&up.server)
            .await;
    }
}

struct Scene {
    _dir: tempfile::TempDir,
    home: Home,
    engine: Arc<Engine>,
    queue: Arc<Queue>,
}

fn scene(up: &Upstream) -> Scene {
    let dir = tempfile::tempdir().unwrap();
    let home = Home::new(dir.path().join("home"));
    let engine = engine(&home, &up.server.uri(), &dir.path().join("apps"), None);
    engine.fetcher.set_patient(true);
    let queue = Queue::open(engine.clone()).unwrap();
    Scene {
        _dir: dir,
        home,
        engine,
        queue,
    }
}

fn add(queue: &Arc<Queue>) -> i64 {
    queue.add(MODEL, vec![], vec![App::Bionic], false).unwrap()
}

async fn settled(queue: &Arc<Queue>, id: i64) -> JobRecord {
    tokio::time::timeout(Duration::from_secs(30), queue.settled(id))
        .await
        .expect("the job settles in time")
        .unwrap()
}

/// Wait until the job has some bytes but not all of them.
async fn under_way(queue: &Arc<Queue>, id: i64) -> JobRecord {
    for _ in 0..600 {
        let record = queue.get(id).unwrap();
        if record.bytes_done > 0 {
            assert_ne!(
                record.state,
                State::Done,
                "the download was too fast to stop"
            );
            return record;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the download never started");
}

fn leftovers(home: &Home) -> Vec<String> {
    fs::read_dir(home.tmp_dir())
        .map(|dir| {
            dir.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn placed(engine: &Engine) -> std::path::PathBuf {
    engine.dirs.bionic.join(REPO).join(WEIGHTS)
}

fn history(record: &JobRecord) -> String {
    record
        .events
        .iter()
        .map(|e| e.text.as_str())
        .collect::<Vec<_>>()
        .join(" | ")
}

#[tokio::test]
async fn a_job_downloads_in_parts_verifies_and_places() {
    let up = upstream(true).await;
    let s = scene(&up);
    let id = add(&s.queue);
    let record = settled(&s.queue, id).await;

    assert_eq!(record.state, State::Done, "{}", history(&record));
    assert_eq!(record.commit.as_deref(), Some(COMMIT));
    assert_eq!(record.bytes_total, record.bytes_done);
    assert_eq!(record.files.len(), 2);
    assert!(record.files.iter().all(|f| f.stage == FileStage::Stored));
    assert_eq!(fs::read(placed(&s.engine)).unwrap(), up.weights);
    let outcome = record.outcome.unwrap();
    assert_eq!(outcome["downloaded"], 2);

    let mut ranges = up.weight_ranges().await;
    ranges.sort();
    assert_eq!(
        ranges,
        [
            "bytes=0-99999",
            "bytes=100000-199999",
            "bytes=200000-299999",
            "bytes=300000-399999"
        ]
    );
    assert!(leftovers(&s.home).is_empty(), "{:?}", leftovers(&s.home));
    // The queue is in the database: a second queue on it sees the job.
    let again = Queue::open(s.engine.clone()).unwrap();
    assert_eq!(again.get(id).unwrap().state, State::Done);
}

#[tokio::test]
async fn parts_that_are_already_there_are_not_fetched_again() {
    let up = upstream(true).await;
    let s = scene(&up);
    let sha = sha256(&up.weights);
    // The first two parts of an earlier attempt, and the note of them.
    let mut partial = up.weights.clone();
    partial[2 * PART_SIZE as usize..].fill(0);
    fs::write(s.engine.store.tmp_path(&format!("{sha}.part")), partial).unwrap();
    fs::write(
        s.engine.store.tmp_path(&format!("{sha}.parts")),
        json!({"size": up.weights.len(), "part": PART_SIZE, "done": [0, 1]}).to_string(),
    )
    .unwrap();

    let id = add(&s.queue);
    let record = settled(&s.queue, id).await;
    assert_eq!(record.state, State::Done, "{}", history(&record));
    assert_eq!(fs::read(placed(&s.engine)).unwrap(), up.weights);
    let mut ranges = up.weight_ranges().await;
    ranges.sort();
    assert_eq!(ranges, ["bytes=200000-299999", "bytes=300000-399999"]);
}

#[tokio::test]
async fn a_busy_server_is_waited_out_and_the_window_is_cut() {
    let up = upstream(false).await;
    // The first two requests for the weights are told to come back later.
    Mock::given(method("GET"))
        .and(path(Upstream::weights_path()))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "1")
                .insert_header("ratelimit", r#""resolvers";r=2000;t=600"#)
                .insert_header(
                    "ratelimit-policy",
                    r#""fixed window";"resolvers";q=3000;w=300"#,
                ),
        )
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&up.server)
        .await;
    serve_files(&up).await;
    let s = scene(&up);
    let started = std::time::Instant::now();
    let id = add(&s.queue);
    let record = settled(&s.queue, id).await;

    assert_eq!(record.state, State::Done, "{}", history(&record));
    assert!(
        history(&record).contains("waiting server: Hugging Face asked for a pause"),
        "{}",
        history(&record)
    );
    assert!(
        started.elapsed() >= Duration::from_secs(1),
        "the pause was kept"
    );
    assert_eq!(fs::read(placed(&s.engine)).unwrap(), up.weights);
    let state = s.engine.governor.state();
    assert_eq!(state.budgets[0].name, "resolvers");
    assert_eq!(state.budgets[0].quota, 3000);
}

#[tokio::test]
async fn pause_keeps_what_is_there_and_resume_finishes() {
    let up = upstream(true).await;
    let s = scene(&up);
    // Slow enough to stop half way.
    s.engine.governor.configure(8, 100_000);
    let id = add(&s.queue);
    under_way(&s.queue, id).await;
    s.queue.pause(id).await.unwrap();

    let paused = s.queue.get(id).unwrap();
    assert_eq!(paused.state, State::Paused);
    let sha = sha256(&up.weights);
    assert!(
        leftovers(&s.home).contains(&format!("{sha}.part")),
        "the partial file is kept: {:?}",
        leftovers(&s.home)
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        s.queue.get(id).unwrap().state,
        State::Paused,
        "it stays paused"
    );

    s.engine.governor.configure(8, 0);
    s.queue.resume(id).unwrap();
    let record = settled(&s.queue, id).await;
    assert_eq!(record.state, State::Done, "{}", history(&record));
    assert_eq!(record.bytes_done, record.bytes_total);
    assert_eq!(fs::read(placed(&s.engine)).unwrap(), up.weights);
    assert!(leftovers(&s.home).is_empty());
}

#[tokio::test]
async fn cancel_removes_what_was_downloaded() {
    let up = upstream(true).await;
    let s = scene(&up);
    s.engine.governor.configure(8, 100_000);
    let id = add(&s.queue);
    under_way(&s.queue, id).await;
    s.queue.cancel(id).await.unwrap();

    let record = s.queue.get(id).unwrap();
    assert_eq!(record.state, State::Cancelled);
    assert!(leftovers(&s.home).is_empty(), "{:?}", leftovers(&s.home));
    assert!(!placed(&s.engine).exists());
    assert!(
        s.queue.pause(id).await.is_err(),
        "a finished job cannot be paused"
    );
    assert_eq!(s.queue.clear_finished().unwrap(), 1);
    assert!(s.queue.get(id).is_err());
}

#[tokio::test]
async fn a_job_that_was_working_continues_after_a_restart() {
    let up = upstream(true).await;
    let s = scene(&up);
    s.engine.governor.configure(8, 100_000);
    let id = add(&s.queue);
    under_way(&s.queue, id).await;

    // The daemon stops without a word: the job is "downloading" in the
    // database, and nothing is running.
    let saved = s.engine.db().downloads().unwrap();
    assert!(
        saved[0].contains(r#""state":"downloading""#),
        "{}",
        saved[0]
    );
    s.queue.pause(id).await.unwrap();
    s.engine
        .db()
        .save_download(id, "downloading", &saved[0])
        .unwrap();
    s.engine.governor.configure(8, 0);

    let restarted = Queue::open(s.engine.clone()).unwrap();
    assert_eq!(restarted.get(id).unwrap().state, State::Queued);
    restarted.start();
    let record = settled(&restarted, id).await;
    assert_eq!(record.state, State::Done, "{}", history(&record));
    assert!(history(&record).contains("continued after the daemon restarted"));
    assert_eq!(fs::read(placed(&s.engine)).unwrap(), up.weights);
}

#[tokio::test]
async fn a_model_the_hub_will_not_show_needs_the_owner() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let up = Upstream {
        server,
        weights: vec![],
        config: vec![],
    };
    let s = scene(&up);
    let id = add(&s.queue);
    let record = settled(&s.queue, id).await;
    assert_eq!(record.state, State::NeedsAttention);
    let why = record.detail.unwrap();
    assert!(why.contains("gated") && why.contains("HTTP 401"), "{why}");

    // Asking again is allowed, and ends the same way.
    s.queue.resume(id).unwrap();
    assert_eq!(settled(&s.queue, id).await.state, State::NeedsAttention);
    assert!(s.queue.add("not a model", vec![], vec![], false).is_err());
}

#[tokio::test]
async fn a_file_that_arrives_damaged_twice_needs_the_owner() {
    let up = upstream(false).await;
    let mut damaged = up.weights.clone();
    damaged[7] ^= 0xff;
    for (file, body) in [(WEIGHTS, &damaged), ("config.json", &up.config)] {
        Mock::given(method("GET"))
            .and(path(format!("/{REPO}/resolve/{COMMIT}/{file}")))
            .respond_with(Ranged(body.clone()))
            .mount(&up.server)
            .await;
    }
    let s = scene(&up);
    let id = add(&s.queue);
    let record = settled(&s.queue, id).await;
    assert_eq!(record.state, State::NeedsAttention, "{}", history(&record));
    assert!(record.detail.unwrap().contains("damaged twice"));
    assert_eq!(
        up.weight_ranges().await.len(),
        8,
        "fetched twice, part by part"
    );
    assert!(leftovers(&s.home).is_empty(), "{:?}", leftovers(&s.home));
    assert!(!Path::new(&placed(&s.engine)).exists());
}

#[tokio::test]
async fn a_job_that_gets_into_none_of_its_apps_needs_the_owner() {
    // The fake Hub has no Ollama manifest for the model, so Ollama cannot
    // be given it; Bionic can.
    let up = upstream(true).await;
    let s = scene(&up);

    let id = s
        .queue
        .add(MODEL, vec![], vec![App::Ollama], false)
        .unwrap();
    let record = settled(&s.queue, id).await;
    assert_eq!(record.state, State::NeedsAttention, "{}", history(&record));
    let detail = record.detail.unwrap_or_default();
    assert!(detail.starts_with("not placed into Ollama: "), "{detail}");
    // The download itself is done and kept.
    assert_eq!(record.bytes_done, record.bytes_total);
    assert!(s.engine.store.contains(&sha256(&up.weights)));

    // With an app that took it, the job is done and the note is in its
    // history.
    let id = s
        .queue
        .add(MODEL, vec![], vec![App::Bionic, App::Ollama], false)
        .unwrap();
    let record = settled(&s.queue, id).await;
    assert_eq!(record.state, State::Done, "{}", history(&record));
    assert!(
        history(&record).contains("not placed into Ollama: "),
        "{}",
        history(&record)
    );
    assert_eq!(fs::read(placed(&s.engine)).unwrap(), up.weights);
}
