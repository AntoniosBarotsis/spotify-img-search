use anyhow::Context;
use clap::{Parser, Subcommand};
use spotify_api::PlaylistId;

#[derive(Parser, Debug)]
#[command(version, about, long_about)]
struct Cli {
  #[command(subcommand)]
  command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
  /// Downloads song thumbnails. By default it fetches your liked/saved songs.
  Download {
    /// Fetches songs from the given playlist.
    #[arg(short, long)]
    playlist_id: Option<String>,

    /// Fetches all songs from all your saved playlists.
    #[arg(long)]
    playlists: bool,
  },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  let cli = Cli::parse();

  match cli.command {
    Some(Commands::Download {
      playlist_id: None,
      playlists: false,
    }) => {
      let spotify = spotify_api::authenticate().await?;
      spotify_api::get_from_liked(spotify).await?;
    }
    Some(Commands::Download {
      playlist_id: Some(playlist_id),
      playlists: false,
    }) => {
      let spotify = spotify_api::authenticate().await?;
      let playlist_id =
        PlaylistId::from_id(playlist_id).context("ID contains invalid characters.")?;
      spotify_api::get_from_playlist(spotify, playlist_id).await?;
    }
    Some(Commands::Download {
      playlist_id: None,
      playlists: true,
    }) => {
      let spotify = spotify_api::authenticate().await?;
      spotify_api::get_from_all_playlists(spotify).await?;
    }
    None => todo!(),
    _ => unreachable!(),
  }

  Ok(())
}
