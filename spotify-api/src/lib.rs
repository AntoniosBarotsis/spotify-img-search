use anyhow::Context;
use base64::Engine;
use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::RetryTransientMiddleware;
use reqwest_retry::policies::ExponentialBackoff;
pub use rspotify::model::PlaylistId;
use rspotify::{
  AuthCodeSpotify, Config, Credentials, OAuth,
  clients::OAuthClient,
  model::{Id, Page, PlayableItem, PlaylistItem, SavedTrack},
  prelude::BaseClient,
  scopes,
};

use tokio::io::AsyncWriteExt;

#[allow(clippy::missing_panics_doc)]
pub async fn authenticate() -> anyhow::Result<AuthCodeSpotify> {
  let creds = Credentials::from_env().expect("Credentials missing");
  let oauth = OAuth::from_env(scopes!("user-library-read")).expect("Cant init oauth");
  let config = Config {
    token_cached: true,
    ..Default::default()
  };
  let spotify = AuthCodeSpotify::with_config(creds, oauth, config);

  let url = spotify.get_authorize_url(false)?;
  spotify.prompt_for_token(&url).await?;
  Ok(spotify)
}

async fn download_image(
  song: Song,
  client: ClientWithMiddleware,
  bar: ProgressBar,
) -> anyhow::Result<()> {
  let encoded = base64::engine::general_purpose::URL_SAFE.encode(song.name.as_bytes());

  let output_path = "./images/".to_owned() + &encoded + "@" + &song.id + ".jpg";
  // TODO: Some songs seem to not have images at all, skipping them for now, see if I want to do
  // something else about them at some point
  if song.images.is_empty() {
    return Ok(());
  }

  // TODO: If image sizes are always ordered, maybe consider using the smaller one?
  let response = client.get(&song.images[0]).send().await?;
  let image_data = response.bytes().await?;

  tokio::fs::create_dir_all("./images/").await?;
  let mut file = tokio::fs::File::create(&output_path).await?;
  file.write_all(&image_data).await?;

  bar.inc(1);

  Ok(())
}

fn create_bar() -> (ProgressBar, ProgressBar) {
  let multi_bar = indicatif::MultiProgress::new();
  let bar1 = ProgressBar::new_spinner();
  bar1.set_style(
    ProgressStyle::with_template("[{elapsed_precise}] {bar:40.cyan/blue} {pos:>7}/{len:7} {msg}")
      .expect("Invalid template")
      .progress_chars("##-"),
  );
  let bar2 = ProgressBar::new_spinner();
  bar2.set_style(
    ProgressStyle::with_template("[{elapsed_precise}] {bar:40.cyan/blue} {pos:>7}/{len:7} {msg}")
      .expect("Invalid template")
      .progress_chars("##-"),
  );

  (multi_bar.add(bar1), multi_bar.add(bar2))
}

async fn process_items<F, Fut, Item>(
  fetch_page: F,
  convert: impl Fn(Item) -> anyhow::Result<Song>,
  list_name: String,
) -> anyhow::Result<()>
where
  F: Fn(u32, u32) -> Fut,
  Fut: Future<Output = anyhow::Result<Page<Item>>>,
  Item: for<'de> serde::de::Deserialize<'de>,
{
  let retry_policy = ExponentialBackoff::builder().build_with_max_retries(3);
  let client = ClientBuilder::new(
    reqwest::ClientBuilder::new()
      .timeout(std::time::Duration::from_secs(10))
      .build()?,
  )
  .with(RetryTransientMiddleware::new_with_policy(retry_policy))
  .build();
  // let client = Client::new();

  let mut handles = Vec::new();

  let (bar1, bar2) = create_bar();
  bar1.set_message(format!("Fetching {list_name}..."));
  bar2.set_message("Downloading thumbnails...");

  let limit = 50;
  let mut offset = 0;

  loop {
    let page = fetch_page(limit, offset)
      .await
      .context("Failed to fetch page")?;

    bar1.set_length(u64::from(page.total));
    bar2.set_length(u64::from(page.total));

    for item in page.items {
      match convert(item) {
        Ok(song) => {
          let task = tokio::spawn(download_image(song, client.clone(), bar2.clone()));
          handles.push(task);
        }
        Err(e) => {
          eprintln!("Skipping item: {e}");
        }
      }
      bar1.inc(1);
    }

    if page.next.is_none() {
      break;
    }
    offset += limit;
  }

  bar1.finish_with_message(format!("Done fetching {list_name}"));

  for task in handles {
    if let Err(e) = task.await? {
      eprintln!("Task error: {e}");
    }
  }

  bar2.finish_with_message("Thumbnails downloaded");
  Ok(())
}

pub async fn get_from_all_playlists(spotify: AuthCodeSpotify) -> anyhow::Result<()> {
  let mut playlists = spotify.current_user_playlists();

  while let Some(playlist) = playlists.next().await {
    let id = playlist?.id;
    get_from_playlist(spotify.clone(), id).await?;
  }

  Ok(())
}

pub async fn get_from_playlist(
  spotify: AuthCodeSpotify,
  playlist_id: PlaylistId<'_>,
) -> anyhow::Result<()> {
  let spotify_clone = spotify.clone();

  let playlist = spotify.playlist(playlist_id.clone(), None, None).await;
  // I allow this here to be easier to find if it does go wrong
  if playlist.is_err() {
    eprintln!(
      "Encountered error for playlist id {}, skipping...",
      &playlist_id
    );

    return Ok(());
  }

  let playlist = playlist?;
  let playlist_name = format!("\"{}\"", playlist.name);

  process_items(
    move |limit, offset| {
      let spotify = spotify_clone.clone();
      let id = playlist_id.clone();
      async move {
        spotify
          .playlist_items_manual(
            id,
            // I wanted to use Some("items(track.id,track.name,track.album.images)")
            // but the library's json parsing doesn't like that.
            None,
            None,
            Some(limit),
            Some(offset),
          )
          .await
          .map_err(Into::into)
      }
    },
    Song::try_from,
    playlist_name,
  )
  .await
}

pub async fn get_from_liked(spotify: AuthCodeSpotify) -> anyhow::Result<()> {
  let spotify_clone = spotify.clone();

  process_items(
    move |limit, offset| {
      let spotify = spotify_clone.clone();
      async move {
        spotify
          .current_user_saved_tracks_manual(None, Some(limit), Some(offset))
          .await
          .map_err(Into::into)
      }
    },
    |item| Ok(Song::from(item)),
    "liked/saved songs".to_owned(),
  )
  .await
}

#[allow(unused)]
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
struct Song {
  id: String,
  name: String,
  artists: Vec<String>,
  images: Vec<String>,
}

impl TryFrom<PlaylistItem> for Song {
  type Error = anyhow::Error;

  fn try_from(value: PlaylistItem) -> Result<Self, Self::Error> {
    if let Some(PlayableItem::Track(item)) = value.track {
      let name = item.name;

      let artists = item
        .artists
        .into_iter()
        .map(|el| el.name)
        .collect::<Vec<_>>();

      let images = item
        .album
        .images
        .into_iter()
        .map(|el| el.url)
        .collect::<Vec<_>>();

      // FIXME: If its local, id wont exist
      let id = item
        .id
        .context("Local songs are not handled yet.")
        .unwrap()
        .id()
        .to_owned();

      // bar.set_message("Fetching ".to_owned() + &name);
      Ok(Self {
        id,
        name,
        artists,
        images,
      })
    } else {
      Err(anyhow::anyhow!("Could not convert {:?}", value.track))
    }
  }
}

// One unwrap used
#[allow(clippy::fallible_impl_from)]
impl From<SavedTrack> for Song {
  fn from(item: SavedTrack) -> Self {
    let name = item.track.name;

    let artists = item
      .track
      .artists
      .into_iter()
      .map(|el| el.name)
      .collect::<Vec<_>>();

    let images = item
      .track
      .album
      .images
      .into_iter()
      .map(|el| el.url)
      .collect::<Vec<_>>();

    // FIXME: If its local, id wont exist
    let id = item
      .track
      .id
      .context("Local songs are not handled yet.")
      .unwrap()
      .id()
      .to_owned();

    // bar.set_message("Fetching ".to_owned() + &name);
    Self {
      id,
      name,
      artists,
      images,
    }
  }
}
