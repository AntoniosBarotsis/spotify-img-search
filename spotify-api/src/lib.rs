use base64::Engine;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::{Client, Request};
use rspotify::{
  AuthCodeSpotify, Config, Credentials, OAuth,
  clients::OAuthClient,
  model::{FullTrack, Id, Page, PlayableItem, PlaylistId, PlaylistItem, SavedTrack},
  prelude::BaseClient,
  scopes,
};

use tokio::io::AsyncWriteExt;

#[allow(clippy::missing_panics_doc)]
pub async fn driver() -> anyhow::Result<()> {
  let creds = Credentials::from_env().expect("Credentials missing");
  let oauth = OAuth::from_env(scopes!("user-library-read")).expect("Cant init oauth");
  let config = Config {
    token_cached: true,
    ..Default::default()
  };
  let spotify = AuthCodeSpotify::with_config(creds, oauth, config);

  let url = spotify.get_authorize_url(false)?;
  spotify.prompt_for_token(&url).await?;

  let client = Client::new();
  get_from_liked(spotify, client).await?;

  Ok(())
}

async fn download_image(song: Song, client: Client) -> anyhow::Result<()> {
  let encoded = base64::engine::general_purpose::URL_SAFE.encode(song.name.as_bytes());

  let output_path = "./images/".to_owned() + &encoded + "@" + &song.id + ".jpg";
  // TODO: If image sizes are always ordered, maybe consider using the smaller one?
  let response = client.get(&song.images[0]).send().await?;
  let image_data = response.bytes().await?;

  tokio::fs::create_dir_all("./images/").await?;
  let mut file = tokio::fs::File::create(&output_path).await?;
  file.write_all(&image_data).await?;

  Ok(())
}

fn create_bar() -> ProgressBar {
  let bar = ProgressBar::new_spinner();
  bar.set_style(
    ProgressStyle::with_template("[{elapsed_precise}] {bar:40.cyan/blue} {pos:>7}/{len:7} {msg}")
      .expect("Invalid template")
      .progress_chars("##-"),
  );
  bar
}

async fn process_items<F, Fut, Item>(
  client: Client,
  fetch_page: F,
  convert: impl Fn(Item) -> anyhow::Result<Song>,
) -> anyhow::Result<()>
where
  F: Fn(u32, u32) -> Fut,
  Fut: Future<Output = anyhow::Result<Page<Item>>>,
  Item: for<'de> serde::de::Deserialize<'de>,
{
  let mut handles = Vec::new();
  let bar = create_bar();
  let limit = 50;
  let mut offset = 0;

  loop {
    let page = fetch_page(limit, offset).await?;

    bar.set_length(u64::from(page.total));
    bar.set_message("Downloading song thumbnails");

    for item in page.items {
      match convert(item) {
        Ok(song) => {
          let task = tokio::spawn(download_image(song, client.clone()));
          handles.push(task);
        }
        Err(e) => {
          eprintln!("Skipping item: {}", e);
        }
      }
      bar.inc(1);
    }

    if page.next.is_none() {
      break;
    }
    offset += limit;
  }

  bar.finish_with_message("Done");

  let bar = create_bar();
  bar.set_length(handles.len().try_into()?);
  bar.set_message("Joining background tasks");

  for task in handles {
    if let Err(e) = task.await? {
      eprintln!("Task error: {e}");
    }
    bar.inc(1);
  }

  bar.finish_with_message("Done");
  Ok(())
}

async fn get_from_playlist(spotify: AuthCodeSpotify, client: Client) -> anyhow::Result<()> {
  // TODO: Don't hardcode the id
  let id = PlaylistId::from_id("22nJr6nQ1OSEXKwipYOZ3j")?;
  let spotify_clone = spotify.clone();

  process_items(
    client,
    move |limit, offset| {
      let spotify = spotify_clone.clone();
      let id = id.clone();
      async move {
        spotify
          .playlist_items_manual(
            id,
            Some("items(track.id,track.name,track.album.images)"),
            None,
            Some(limit),
            Some(offset),
          )
          .await
          .map_err(Into::into)
      }
    },
    Song::try_from,
  )
  .await
}

async fn get_from_liked(spotify: AuthCodeSpotify, client: Client) -> anyhow::Result<()> {
  let spotify_clone = spotify.clone();

  process_items(
    client,
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
      let id = item.id.unwrap().id().to_owned();

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
    let id = item.track.id.unwrap().id().to_owned();

    // bar.set_message("Fetching ".to_owned() + &name);
    Self {
      id,
      name,
      artists,
      images,
    }
  }
}
