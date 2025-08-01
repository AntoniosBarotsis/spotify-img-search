// TODO: make this a CLI, have a command for downloading playlists instead

use std::collections::HashSet;

use base64::Engine;
use indicatif::{ProgressBar, ProgressStyle};
use rspotify::{
  AuthCodeSpotify, Config, Credentials, OAuth, clients::OAuthClient, model::Id, scopes,
};

use tokio::io::AsyncWriteExt; // For write_all()

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  let creds = Credentials::from_env().expect("Credentials missing");
  let oauth = OAuth::from_env(scopes!("user-library-read")).expect("Cant init oauth");
  let config = Config {
    token_cached: true,
    ..Default::default()
  };
  let spotify = AuthCodeSpotify::with_config(creds, oauth, config);

  let url = spotify.get_authorize_url(false)?;
  spotify.prompt_for_token(&url).await?;

  // TODO: Doing a 2 pass here takes a while for no reason. Move the image stuff in the same
  // fetch func
  let songs = get_from_liked(spotify).await?;

  let bar = ProgressBar::new_spinner();
  bar.set_style(
    ProgressStyle::with_template("[{elapsed_precise}] {bar:40.cyan/blue} {pos:>7}/{len:7} {msg}")
      .expect("Invalid template")
      .progress_chars("##-"),
  );
  bar.set_length(songs.iter().len().try_into().unwrap());

  for song in songs {
    bar.inc(1);
    bar.set_message("Downloading thumbnail ".to_owned() + &song.name);

    let encoded = base64::engine::general_purpose::URL_SAFE.encode(song.name.as_bytes());

    let output_path = "./images/".to_owned() + &encoded + "@" + &song.id + ".jpg";
    // TODO: If image sizes are always ordered, maybe consider using the smaller one?
    let response = reqwest::get(&song.images[0]).await?;
    let image_data = response.bytes().await?;

    tokio::fs::create_dir_all("./images/").await?;
    let mut file = tokio::fs::File::create(&output_path).await?;
    file.write_all(&image_data).await?;
  }

  Ok(())
}

async fn get_from_liked(spotify: AuthCodeSpotify) -> anyhow::Result<HashSet<Song>> {
  let mut songs = HashSet::<Song>::new();

  let bar = ProgressBar::new_spinner();
  bar.set_style(
    ProgressStyle::with_template("[{elapsed_precise}] {bar:40.cyan/blue} {pos:>7}/{len:7} {msg}")
      .expect("Invalid template")
      .progress_chars("##-"),
  );

  let limit = 50;
  let mut offset = 0;

  loop {
    let page = spotify
      .current_user_saved_tracks_manual(None, Some(limit), Some(offset))
      .await?;

    bar.set_length(u64::from(page.total));
    bar.set_message("Fetching song metadata");

    for item in page.items {
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

      bar.inc(1);
      // bar.set_message("Fetching ".to_owned() + &name);
      let _ = songs.insert(Song {
        id,
        name,
        artists,
        images,
      });
    }

    // The iteration ends when the `next` field is `None`. Otherwise, the
    // Spotify API will keep returning empty lists from then on.
    if page.next.is_none() {
      break;
    }

    offset += limit;
  }

  bar.finish_with_message("Done");

  Ok(songs)
}

#[allow(unused)]
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
struct Song {
  id: String,
  name: String,
  artists: Vec<String>,
  images: Vec<String>,
}
