use std::{thread, time};

use discord_rich_presence::activity;
use discord_rich_presence::{DiscordIpc, DiscordIpcClient as DiscordClient};
use mpd_client::commands::responses::{PlayState, Song};
use mpd_client::{commands, Client as MPDClient};
use regex::Regex;

use crate::album_art::AlbumArtClient;
use crate::mpd_conn::get_timestamp;
use config::Config;
use defaults::{ACTIVE_TIME, IDLE_TIME};

mod album_art;
mod config;
mod defaults;
mod mpd_conn;

/// Attempts to find a playing MPD host every 5
/// seconds until one is found
async fn idle(hosts: &[String]) -> MPDClient {
    println!("Entering idle mode");

    loop {
        let conn_wrapper = mpd_conn::try_get_mpd_conn(hosts).await;

        if let Some(client) = conn_wrapper {
            println!("Exiting idle mode");
            return client;
        }

        thread::sleep(time::Duration::from_secs(IDLE_TIME));
    }
}

#[tokio::main]
async fn main() {
    let re = Regex::new(r"\$(\w+)").unwrap();

    // Load config and defaults if necessary.
    // We're safe to unwrap everything here since all options should have valid defaults.
    let config = Config::load();
    let id = config.id.unwrap();
    let hosts = &config.hosts.unwrap();
    let format_options = config.format.unwrap();
    let (
        details_format,
        state_format,
        timestamp_mode,
        large_image,
        small_image,
        large_text_format,
        small_text_format,
    ) = (
        format_options.details.as_deref().unwrap(),
        format_options.state.as_deref().unwrap(),
        format_options.timestamp.as_deref().unwrap(),
        format_options.large_image.as_deref().unwrap(),
        format_options.small_image.as_deref().unwrap(),
        format_options.large_text.as_deref().unwrap(),
        format_options.small_text.as_deref().unwrap(),
    );

    let details_tokens = get_tokens(&re, details_format);
    let state_tokens = get_tokens(&re, state_format);
    let large_text_tokens = get_tokens(&re, large_text_format);
    let small_text_tokens = get_tokens(&re, state_format);

    // MPD and Discord connections
    let mut mpd = idle(hosts).await;
    let drpc_w = DiscordClient::new(&id.to_string());

    if let Err(ref why) = drpc_w {
        eprintln!("Failed to create a new client: {}", why);
    };

    let mut drpc = drpc_w.unwrap();

    if let Err(why) = drpc.connect() {
        eprintln!("Failed to connect the client {}", why);
    };

    let mut album_art_client = AlbumArtClient::new();

    // Main program loop - keep updating state until exit
    loop {
        let state = mpd_conn::get_status(&mpd).await.state;

        if state == PlayState::Playing {
            let current_song = mpd.command(commands::CurrentSong).await;
            if let Ok(Some(song_in_queue)) = current_song {
                let song = song_in_queue.song;

                let details =
                    replace_tokens(details_format, &details_tokens, &song, &mut mpd).await;
                let state = replace_tokens(state_format, &state_tokens, &song, &mut mpd).await;
                let large_text =
                    replace_tokens(large_text_format, &large_text_tokens, &song, &mut mpd).await;
                let small_text =
                    replace_tokens(small_text_format, &small_text_tokens, &song, &mut mpd).await;

                let timestamps = get_timestamp(&mut mpd, timestamp_mode).await;

                let mut assets = activity::Assets::new();

                let mut large_image_str: String = String::new();

                let url = album_art_client.get_album_art_url(song.clone());
                match url {
                    Some(url) => large_image_str = url,
                    None => {
                        if !large_image.is_empty() {
                            large_image_str = large_image.to_owned();
                        }
                    }
                };

                if !large_image_str.is_empty() {
                    assets = assets.large_image(&large_image_str);
                }
                if !small_image.is_empty() {
                    assets = assets.small_image(small_image)
                }
                if !large_text.is_empty() {
                    assets = assets.large_text(&large_text)
                }
                if !small_text.is_empty() {
                    assets = assets.small_text(&small_text)
                }

                let mut buttons: Vec<activity::Button> = vec![];

                let mut release_str: String = String::new();
                if let Some(release) = album_art_client.get_album_release_url(song.clone()) {
                    release_str = release;
                };

                if !release_str.is_empty() {
                    buttons.push(activity::Button::new("MusicBrainz", &release_str));
                }

                let res = drpc.set_activity(
                    activity::Activity::new()
                        .state(&state)
                        .details(&details)
                        .assets(assets)
                        .timestamps(timestamps)
                        .buttons(buttons),
                );

                if let Err(why) = res {
                    eprintln!("Failed to set activity: {}", why);
                };
            }
        } else {
            if let Err(why) = drpc.clear_activity() {
                eprintln!("Failed to clear activity: {}", why);
            };

            mpd = idle(hosts).await;
        }

        // sleep for 1 sec to not hammer the mpd and rpc servers
        thread::sleep(time::Duration::from_secs(ACTIVE_TIME));
    }
}

/// Extracts the formatting tokens from a formatting string
fn get_tokens(re: &Regex, format_string: &str) -> Vec<String> {
    re.captures_iter(format_string)
        .map(|caps| caps[1].to_string())
        .collect::<Vec<_>>()
}

/// Replaces each of the formatting tokens in the formatting string
/// with actual data pulled from MPD
async fn replace_tokens(
    format_string: &str,
    tokens: &Vec<String>,
    song: &Song,
    mpd: &mut MPDClient,
) -> String {
    let mut compiled_string = format_string.to_string();
    for token in tokens {
        let value = mpd_conn::get_token_value(mpd, song, token).await;
        compiled_string = compiled_string.replace(format!("${}", token).as_str(), value.as_str());
    }
    compiled_string
}
