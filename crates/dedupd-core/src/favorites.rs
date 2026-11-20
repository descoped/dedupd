//! The models the owner marked as favorites, and the likes on Hugging Face
//! brought in among them.
//!
//! Kept in `<home>/favorites.json` rather than the database, so the schema
//! stays as it is and a reset of the database keeps them. A missing or
//! unreadable file means there are none. Only the daemon writes it, one
//! change at a time; the file is replaced in one step.
//!
//! Likes are read, never written: Hugging Face does not let a program like
//! a model. A like brought in is a favorite like any other. Taking the star
//! off one in dedupd is remembered, so the next reading does not bring it
//! back; a like that is taken back on Hugging Face goes from the favorites
//! too, unless it was starred in dedupd.

use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::hub::HubClient;
use crate::paths::Home;
use dedupd_api::favorites::{Favorite, FavoriteSource, Favorites, LikesSync};

/// How long the likes read are taken as current.
pub const LIKES_FRESH_FOR: i64 = 3600;

/// What the file holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kept {
    #[serde(default)]
    pub favorites: Vec<Favorite>,
    /// Likes the owner took the star off in dedupd.
    #[serde(default)]
    pub dismissed: Vec<String>,
    /// The account whose likes were read.
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub synced_at: Option<i64>,
    #[serde(default)]
    pub problem: Option<String>,
}

impl Kept {
    /// What is kept in `home`; nothing when the file is missing or cannot
    /// be read.
    pub fn load(home: &Home) -> Self {
        std::fs::read(home.favorites_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// What is kept, for a change that will be written back. A file that
    /// is there and cannot be read is not written over: it is moved aside
    /// first (`favorites.unreadable.json`), so what it held can still be
    /// looked at, and the change starts from nothing.
    fn load_to_change(home: &Home) -> Result<Self> {
        let path = home.favorites_path();
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        match serde_json::from_slice(&bytes) {
            Ok(kept) => Ok(kept),
            Err(e) => {
                // A name that is free, so an earlier one set aside stays.
                let aside = (1u32..)
                    .map(|n| match n {
                        1 => "favorites.unreadable.json".to_string(),
                        n => format!("favorites.unreadable-{n}.json"),
                    })
                    .map(|name| home.root().join(name))
                    .find(|candidate| !candidate.exists())
                    .expect("a free name among four thousand million");
                std::fs::rename(&path, &aside)
                    .with_context(|| format!("moving {} aside", path.display()))?;
                eprintln!(
                    "dedupd: {} could not be read ({e}); it was moved to {} and the favorites start again",
                    path.display(),
                    aside.display()
                );
                Ok(Self::default())
            }
        }
    }

    fn save(&self, home: &Home) -> Result<()> {
        let target = home.favorites_path();
        let tmp = home.root().join(".favorites.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &target).with_context(|| format!("writing {}", target.display()))
    }

    /// The control API's answer: newest first.
    pub fn answer(&self, has_token: bool) -> Favorites {
        let mut favorites = self.favorites.clone();
        favorites.sort_by(|a, b| b.added_at.cmp(&a.added_at).then(a.repo.cmp(&b.repo)));
        Favorites {
            favorites,
            likes: LikesSync {
                has_token,
                user: self.user.clone(),
                synced_at: self.synced_at,
                problem: self.problem.clone(),
            },
        }
    }

    /// Whether the likes are due to be read again at `now`.
    pub fn likes_due(&self, now: i64) -> bool {
        self.synced_at
            .is_none_or(|at| now - at >= LIKES_FRESH_FOR || at > now)
    }

    /// Bring in what `user` likes on Hugging Face, as read at `now`.
    fn take_likes(&mut self, user: &str, liked: &[String], now: i64) {
        // Another account: what the last one liked is not the owner's now.
        if self.user.as_deref() != Some(user) {
            self.favorites
                .retain(|f| f.source != FavoriteSource::HuggingFace);
            self.dismissed.clear();
        }
        let liked: HashSet<&str> = liked.iter().map(String::as_str).collect();
        self.favorites
            .retain(|f| f.source != FavoriteSource::HuggingFace || liked.contains(f.repo.as_str()));
        self.dismissed.retain(|r| liked.contains(r.as_str()));
        let have: HashSet<String> = self.favorites.iter().map(|f| f.repo.clone()).collect();
        let mut new: Vec<&str> = liked
            .iter()
            .copied()
            .filter(|r| !have.contains(*r) && !self.dismissed.iter().any(|d| d == r))
            .collect();
        new.sort_unstable();
        self.favorites.extend(new.into_iter().map(|repo| Favorite {
            repo: repo.to_string(),
            added_at: now,
            source: FavoriteSource::HuggingFace,
        }));
        self.user = Some(user.to_string());
        self.synced_at = Some(now);
        self.problem = None;
    }
}

/// Mark `repo` as a favorite, or no longer, at `now`.
pub fn set(home: &Home, repo: &str, favorite: bool, now: i64) -> Result<Kept> {
    if !crate::names::is_repo_name(repo) {
        bail!("{repo:?} is not a model's name");
    }
    let mut kept = Kept::load_to_change(home)?;
    let at = kept.favorites.iter().position(|f| f.repo == repo);
    match (favorite, at) {
        (true, None) => {
            kept.dismissed.retain(|r| r != repo);
            kept.favorites.push(Favorite {
                repo: repo.to_string(),
                added_at: now,
                source: FavoriteSource::Dedupd,
            });
        }
        (false, Some(i)) => {
            if kept.favorites.remove(i).source == FavoriteSource::HuggingFace {
                kept.dismissed.push(repo.to_string());
            }
        }
        _ => return Ok(kept),
    }
    kept.save(home)?;
    Ok(kept)
}

/// What a reading of the likes found: the account and what it likes.
pub type Likes = Result<(String, Vec<String>)>;

/// Read the likes of the account the token belongs to. The account is asked
/// for when `known` is `None`, or with `by_hand`; Hugging Face limits how
/// often it may be asked. `None` when there is no token.
pub async fn read_likes(hub: &HubClient, known: Option<String>, by_hand: bool) -> Option<Likes> {
    if !hub.has_token() {
        return None;
    }
    Some(
        async {
            let user = match known {
                Some(user) if !by_hand => user,
                _ => hub.whoami().await?.ok_or_else(|| {
                    anyhow::anyhow!("the Hugging Face token is not a person's, so it has no likes")
                })?,
            };
            let liked = hub.liked_models(&user).await?;
            anyhow::Ok((user, liked))
        }
        .await,
    )
}

/// Keep what `read_likes` found at `now`. On a failure the favorites stay
/// as they were and the reason is kept, to be shown.
pub fn record_likes(home: &Home, likes: Likes, now: i64) -> Result<Kept> {
    let mut kept = Kept::load_to_change(home)?;
    match likes {
        Ok((user, liked)) => kept.take_likes(&user, &liked, now),
        Err(e) => {
            kept.synced_at = Some(now);
            kept.problem = Some(format!("Your likes could not be read: {e:#}."));
        }
    }
    kept.save(home)?;
    Ok(kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repos(kept: &Kept) -> Vec<(&str, FavoriteSource)> {
        let mut all: Vec<_> = kept
            .favorites
            .iter()
            .map(|f| (f.repo.as_str(), f.source))
            .collect();
        all.sort_by_key(|(r, _)| *r);
        all
    }

    #[test]
    fn a_file_that_cannot_be_read_is_moved_aside_not_written_over() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path().to_path_buf());
        std::fs::create_dir_all(home.root()).unwrap();
        set(&home, "org/a", true, 10).unwrap();
        // Half a file, as a full disk or a crash can leave it.
        let whole = std::fs::read(home.favorites_path()).unwrap();
        std::fs::write(home.favorites_path(), &whole[..whole.len() / 2]).unwrap();

        set(&home, "org/b", true, 20).unwrap();
        assert_eq!(Kept::load(&home).favorites.len(), 1);
        let aside = home.root().join("favorites.unreadable.json");
        assert_eq!(std::fs::read(&aside).unwrap(), &whole[..whole.len() / 2]);

        // It happens again: the first one set aside is kept.
        std::fs::write(home.favorites_path(), b"{").unwrap();
        set(&home, "org/c", true, 30).unwrap();
        assert_eq!(std::fs::read(&aside).unwrap(), &whole[..whole.len() / 2]);
        let second = home.root().join("favorites.unreadable-2.json");
        assert_eq!(std::fs::read(second).unwrap(), b"{");
    }

    #[test]
    fn a_star_is_kept_and_taken_off() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path().to_path_buf());
        std::fs::create_dir_all(home.root()).unwrap();
        set(&home, "org/a", true, 10).unwrap();
        set(&home, "org/a", true, 20).unwrap();
        let kept = Kept::load(&home);
        assert_eq!(kept.favorites.len(), 1);
        assert_eq!(kept.favorites[0].added_at, 10);
        set(&home, "org/a", false, 30).unwrap();
        assert!(Kept::load(&home).favorites.is_empty());
        assert!(set(&home, "../etc", true, 40).is_err());
    }

    #[test]
    fn likes_come_in_go_with_the_like_and_stay_off_once_taken_off() {
        use FavoriteSource::*;
        let dir = tempfile::tempdir().unwrap();
        let home = Home::new(dir.path().to_path_buf());
        std::fs::create_dir_all(home.root()).unwrap();
        set(&home, "org/mine", true, 1).unwrap();
        set(&home, "org/both", true, 1).unwrap();
        let mut kept = Kept::load(&home);
        let liked = |r: &[&str]| r.iter().map(|s| s.to_string()).collect::<Vec<_>>();

        kept.take_likes("ove", &liked(&["org/a", "org/b", "org/both"]), 100);
        assert_eq!(
            repos(&kept),
            [
                ("org/a", HuggingFace),
                ("org/b", HuggingFace),
                ("org/both", Dedupd),
                ("org/mine", Dedupd)
            ]
        );
        kept.save(&home).unwrap();

        // Taken off in dedupd: not brought back by the next reading.
        let mut kept = set(&home, "org/a", false, 110).unwrap();
        kept.take_likes("ove", &liked(&["org/a", "org/b", "org/both"]), 200);
        assert_eq!(repos(&kept).len(), 3);
        // Unliked on Hugging Face: gone, but a star of dedupd's own stays.
        kept.take_likes("ove", &liked(&["org/a"]), 300);
        assert_eq!(repos(&kept), [("org/both", Dedupd), ("org/mine", Dedupd)]);
        // Liked again after it was unliked there: it comes back.
        kept.take_likes("ove", &liked(&["org/b"]), 400);
        kept.take_likes("ove", &liked(&["org/a", "org/b"]), 500);
        assert!(repos(&kept).contains(&("org/a", HuggingFace)));
        // Another account: the first one's likes go.
        kept.take_likes("someone", &liked(&["org/c"]), 600);
        assert_eq!(
            repos(&kept),
            [
                ("org/both", Dedupd),
                ("org/c", HuggingFace),
                ("org/mine", Dedupd)
            ]
        );
        assert!(kept.likes_due(600 + LIKES_FRESH_FOR));
        assert!(!kept.likes_due(601));
    }
}
