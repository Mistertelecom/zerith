//! The Media screen without a GPU or a network: a fake catalogue behind the
//! same `Backend` the real OpenRouter sits behind. Search state machine
//! (debounce, paging, cache, stale answers), the fixed-parameter form, the
//! saved settings and the key handling.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use zerith_providers::media::{Kind, ParamKind};
use zerith_ui::screens::media::{self, demo, KeySource, MediaState, PAGE};
use zerith_ui::{spec, Load, Screen, Shell};

fn tempdir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .expect("tempdir")
}

/// A shell whose Media screen reads the fake catalogue. Inline workers: each
/// transition has landed by the next line.
fn shell(
    home: &std::path::Path,
    key: Option<KeySource>,
) -> (Shell, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let app = rune::App::new(spec());
    let mut s = Shell::open(home.join("zerith.db"), app.handle());
    s.inline = true;
    let (backend, calls) = demo::backend(key);
    s.media = MediaState::with_backend(backend, String::new());
    (s, calls)
}

#[test]
fn opening_the_screen_reads_the_key_and_the_saved_models_once() {
    let dir = tempdir();
    let (mut s, calls) = shell(dir.path(), Some(KeySource::Keyring));
    s.media.image.model_id = demo::model(Kind::Image, 6).id;
    s.navigate(Screen::Media);
    assert_eq!(s.media.key_state, Load::Ready(Some(KeySource::Keyring)));
    let m = s
        .media
        .image
        .current
        .ready()
        .expect("current model resolved");
    assert_eq!(m.id, demo::model(Kind::Image, 6).id);
    assert!(!m.price_hint.is_empty());
    let n = calls.load(Ordering::SeqCst);
    s.navigate(Screen::Chat);
    s.navigate(Screen::Media);
    assert_eq!(calls.load(Ordering::SeqCst), n, "reopening asks nothing");
}

#[test]
fn a_search_pages_thirty_at_a_time_and_load_more_appends() {
    let dir = tempdir();
    let (mut s, _) = shell(dir.path(), None);
    s.media.image.query = rune::TextEdit::new("");
    media::start_search(&mut s, Kind::Image);
    let slot = &s.media.image;
    assert_eq!(slot.rows.len(), PAGE);
    assert_eq!(
        slot.total, 240,
        "the whole catalogue matches an empty query"
    );
    assert!(slot.has_more());
    media::load_more(&mut s, Kind::Image);
    assert_eq!(s.media.image.rows.len(), 2 * PAGE);
    // The ids are unique: page two did not repeat page one.
    let mut ids: Vec<&str> = s.media.image.rows.iter().map(|m| m.id.as_str()).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 2 * PAGE);
    for _ in 0..10 {
        media::load_more(&mut s, Kind::Image);
    }
    assert_eq!(s.media.image.rows.len(), 240);
    assert!(!s.media.image.has_more(), "no `Carregar mais` past the end");
}

#[test]
fn every_kind_query_page_is_fetched_once_per_run() {
    let dir = tempdir();
    let (mut s, calls) = shell(dir.path(), None);
    s.media.image.query = rune::TextEdit::new("flux");
    media::search_now(&mut s, Kind::Image);
    let after_first = calls.load(Ordering::SeqCst);
    assert_eq!(after_first, 1);
    let first_rows = s.media.image.rows.clone();

    s.media.image.query = rune::TextEdit::new("seedream");
    media::search_now(&mut s, Kind::Image);
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // Back to the first query: served from the cache.
    s.media.image.query = rune::TextEdit::new("flux");
    media::search_now(&mut s, Kind::Image);
    assert_eq!(calls.load(Ordering::SeqCst), 2, "a repeat is a cache hit");
    assert_eq!(s.media.image.rows, first_rows);
    assert_eq!(media::requests(&s.media), 2);

    // The same text for the other kind is a different key.
    s.media.video.query = rune::TextEdit::new("flux");
    media::search_now(&mut s, Kind::Video);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn a_stale_answer_never_replaces_a_newer_search() {
    let dir = tempdir();
    let (mut s, _) = shell(dir.path(), None);
    s.media.image.query = rune::TextEdit::new("flux");
    media::start_search(&mut s, Kind::Image);
    let (old_seq, old_q) = (s.media.image.seq, "flux");
    let old_page = s.media.cache[&(Kind::Image, old_q.to_string(), 0)].clone();

    s.media.image.query = rune::TextEdit::new("seedream");
    media::start_search(&mut s, Kind::Image);
    let fresh = s.media.image.rows.clone();
    assert!(s.media.image.seq > old_seq);

    // The answer to the first search arrives late, after the second landed.
    media::land_page(&mut s, Kind::Image, old_seq, old_q, 0, Ok(old_page.clone()));
    assert_eq!(s.media.image.rows, fresh, "the late answer was dropped");
    assert_eq!(s.media.image.applied.as_deref(), Some("seedream"));

    // A late failure does not paint an error over good rows either.
    media::land_page(&mut s, Kind::Image, old_seq, old_q, 0, Err("tarde".into()));
    assert_eq!(s.media.image.status, Load::Ready(()));

    // The same query asked again gets a new sequence, so only its own answer lands.
    s.media.image.query = rune::TextEdit::new("flux");
    media::start_search(&mut s, Kind::Image);
    assert_eq!(s.media.image.rows, old_page.models);
}

#[test]
fn a_page_that_is_not_the_next_one_is_dropped() {
    let dir = tempdir();
    let (mut s, _) = shell(dir.path(), None);
    media::start_search(&mut s, Kind::Image);
    let seq = s.media.image.seq;
    let page = s.media.cache[&(Kind::Image, String::new(), 0)].clone();
    // Page 0 again (a double `Carregar mais` race) must not duplicate rows.
    media::land_page(&mut s, Kind::Image, seq, "", 0, Ok(page));
    assert_eq!(s.media.image.rows.len(), PAGE);
}

#[test]
fn an_empty_result_and_a_failure_are_states_not_blanks() {
    let dir = tempdir();
    let (mut s, _) = shell(dir.path(), None);
    s.media.image.query = rune::TextEdit::new("zzzz-nada");
    media::search_now(&mut s, Kind::Image);
    assert_eq!(s.media.image.status, Load::Ready(()));
    assert!(s.media.image.rows.is_empty());
    assert!(!s.media.image.has_more());

    // A backend that fails: the error is kept and can be retried.
    let (mut backend, _) = demo::backend(None);
    backend.models = std::sync::Arc::new(|_, _, _, _| Err("sem rede".to_string()));
    s.media = MediaState::with_backend(backend, String::new());
    media::search_now(&mut s, Kind::Image);
    assert_eq!(s.media.image.status, Load::Failed("sem rede".into()));
    assert!(s.media.cache.is_empty(), "a failure is not cached");
}

#[test]
fn typing_waits_for_the_debounce_and_searches_once() {
    let dir = tempdir();
    let (mut s, calls) = shell(dir.path(), None);
    s.media.set_debounce(Duration::from_millis(40));
    s.media.image.query = rune::TextEdit::new("flux");
    // Three quick keystrokes: one timer, restarted.
    for _ in 0..3 {
        media::edit_query(&mut s, Kind::Image);
    }
    assert!(!media::poll(&mut s), "nothing due yet");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "typing alone asks nothing");
    let end = Instant::now() + Duration::from_secs(3);
    let mut searched = false;
    while Instant::now() < end {
        if media::poll(&mut s) {
            searched = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(searched, "the timer landed");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one search for the burst");
    assert_eq!(s.media.image.applied.as_deref(), Some("flux"));
    assert!(!media::poll(&mut s), "and the timer is spent");
}

#[test]
fn enter_searches_at_once_and_drops_a_pending_timer() {
    let dir = tempdir();
    let (mut s, calls) = shell(dir.path(), None);
    s.media.set_debounce(Duration::from_millis(30));
    s.media.image.query = rune::TextEdit::new("flux");
    media::edit_query(&mut s, Kind::Image);
    media::search_now(&mut s, Kind::Image);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    std::thread::sleep(Duration::from_millis(120));
    assert!(
        !media::poll(&mut s),
        "the cancelled timer does not search again"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// ------------------------------------------------------------- parameters

#[test]
fn the_form_follows_the_models_descriptors() {
    // Index 6: even (aspect_ratio + n), multiple of 3 (quality), not of 4.
    let m = demo::model(Kind::Image, 6);
    let draft = media::Draft::of(&m, &Default::default());
    let kinds: Vec<(&str, &ParamKind)> = draft
        .items
        .iter()
        .map(|i| (i.name.as_str(), &i.kind))
        .collect();
    assert!(matches!(kinds[0], ("aspect_ratio", ParamKind::Enum(_))));
    assert!(matches!(kinds[1], ("n", ParamKind::Range { .. })));
    assert!(matches!(kinds[2], ("quality", ParamKind::Enum(_))));
}

#[test]
fn the_form_becomes_the_saved_map_with_typed_values() {
    let m = demo::model(Kind::Image, 12); // aspect_ratio, n, quality, seed
    let mut d = media::Draft::of(&m, &Default::default());
    for it in &mut d.items {
        match it.name.as_str() {
            "aspect_ratio" => it.choice = Some("16:9".into()),
            "quality" => it.choice = Some("high".into()),
            "n" => it.edit = rune::TextEdit::new("2"),
            "seed" => it.edit = rune::TextEdit::new("42"),
            _ => {}
        }
    }
    let map = d.to_map(&m).expect("valid");
    assert_eq!(map["aspect_ratio"], json!("16:9"));
    assert_eq!(map["quality"], json!("high"));
    assert_eq!(map["n"], json!(2), "a range comes out as a JSON integer");
    assert_eq!(map["seed"], json!(42), "seed is an integer too");
    assert_eq!(map.len(), 4);
    assert_eq!(
        media::summary(&map),
        "aspect_ratio 16:9 · n 2 · quality high · seed 42"
    );
}

#[test]
fn a_bad_value_is_refused_by_name_and_nothing_is_written() {
    let m = demo::model(Kind::Image, 6);
    let mut d = media::Draft::of(&m, &Default::default());
    d.items[1].edit = rune::TextEdit::new("9"); // n is 1..4
    let e = d.to_map(&m).expect_err("out of range");
    assert!(e.starts_with("n:"), "{e}");
    d.items[1].edit = rune::TextEdit::new("abc");
    assert!(d.to_map(&m).is_err());
    d.items[1].edit = rune::TextEdit::new("");
    assert!(d.to_map(&m).expect("empty means unset").is_empty());
}

#[test]
fn flags_and_unset_choices() {
    let m = demo::model(Kind::Video, 2); // duration, resolution, generate_audio
    let mut d = media::Draft::of(&m, &Default::default());
    let audio = d
        .items
        .iter_mut()
        .find(|i| i.name == "generate_audio")
        .unwrap();
    audio.flag = Some(true);
    let map = d.to_map(&m).unwrap();
    assert_eq!(map.len(), 1, "untouched choices are not written");
    assert_eq!(map["generate_audio"], json!(true));
}

#[test]
fn changing_the_model_drops_what_it_cannot_take_and_says_so() {
    let dir = tempdir();
    let (mut s, _) = shell(dir.path(), None);
    let from = demo::model(Kind::Image, 12);
    s.media.image.model_id = from.id.clone();
    s.media.image.saved = [
        ("aspect_ratio".to_string(), json!("16:9")),
        ("n".to_string(), json!(2)),
        ("quality".to_string(), json!("high")),
        ("seed".to_string(), json!(7)),
    ]
    .into_iter()
    .collect();
    // Index 1 is odd: no aspect_ratio, no n, no quality, no seed.
    let to = demo::model(Kind::Image, 1);
    media::use_model(&mut s, to.clone());
    let slot = &s.media.image;
    assert_eq!(slot.model_id, to.id);
    assert!(slot.saved.is_empty(), "{:?}", slot.saved);
    assert_eq!(slot.notes.len(), 4, "one note per dropped parameter");
    assert!(slot.notes[0].contains("foi removido"));
    assert!(!slot.picking, "the picker closes on `Usar`");

    // A model that takes the same name with other options drops the value.
    let mut other = demo::model(Kind::Image, 6);
    other.params[0].kind = ParamKind::Enum(vec!["1:1".into()]);
    s.media.image.saved = [("aspect_ratio".to_string(), json!("16:9"))]
        .into_iter()
        .collect();
    media::use_model(&mut s, other);
    assert!(s.media.image.saved.is_empty());
    assert!(s.media.image.notes[0].contains("16:9"));

    // A compatible value survives, retyped.
    s.media.image.saved = [("n".to_string(), json!("2"))].into_iter().collect();
    media::use_model(&mut s, demo::model(Kind::Image, 6));
    assert_eq!(s.media.image.saved["n"], json!(2));
    assert!(s.media.image.notes.is_empty());
}

#[test]
fn model_and_params_persist_to_settings_toml() {
    let dir = tempdir();
    let (mut s, _) = shell(dir.path(), None);
    let m = demo::model(Kind::Image, 6);
    media::use_model(&mut s, m.clone());
    s.media.image.draft.items[0].choice = Some("1:1".into());
    media::save_params(&mut s, Kind::Image);
    assert_eq!(s.media.image.notice, Some(Ok("Salvo".into())));

    let text = std::fs::read_to_string(dir.path().join("settings.toml")).expect("settings");
    assert!(text.contains(&m.id), "{text}");
    assert!(text.contains("aspect_ratio"), "{text}");

    // A fresh window reads it back.
    let reopened = MediaState::open(dir.path());
    assert_eq!(reopened.image.model_id, m.id);
    assert_eq!(reopened.image.saved["aspect_ratio"], Value::from("1:1"));
    assert!(reopened.video.model_id.is_empty());
}

// -------------------------------------------------------------------- key

#[test]
fn saving_the_key_wipes_the_field_and_never_shows_it_again() {
    let dir = tempdir();
    let (mut s, _) = shell(dir.path(), None);
    s.navigate(Screen::Media);
    assert_eq!(s.media.key_state, Load::Ready(None));
    assert!(s.media.key.is_secure(), "the field is masked");

    let saved = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let sink = saved.clone();
    s.media.backend.save_key = std::sync::Arc::new(move |k| {
        sink.lock().unwrap().push(k.to_string());
        Ok(())
    });
    s.media.key = rune::TextEdit::new("  sk-or-v1-secret  ").secure();
    media::save_key(&mut s);
    assert_eq!(saved.lock().unwrap().as_slice(), ["sk-or-v1-secret"]);
    assert!(s.media.key.text.is_empty(), "the field no longer holds it");
    assert!(s.media.key.is_secure());
    assert_eq!(s.media.key_state, Load::Ready(Some(KeySource::Keyring)));
    assert_eq!(s.media.key_notice, Some(Ok("chave salva no cofre".into())));
    // Nothing on disk mentions it.
    let on_disk = std::fs::read_to_string(dir.path().join("settings.toml")).unwrap_or_default();
    assert!(!on_disk.contains("secret"));
}

#[test]
fn an_empty_key_is_refused_and_a_keyring_failure_is_reported() {
    let dir = tempdir();
    let (mut s, _) = shell(dir.path(), None);
    media::save_key(&mut s);
    assert!(matches!(s.media.key_notice, Some(Err(_))));

    s.media.backend.save_key = std::sync::Arc::new(|_| Err("cofre indisponível".into()));
    s.media.key = rune::TextEdit::new("sk-x").secure();
    media::save_key(&mut s);
    let Some(Err(e)) = &s.media.key_notice else {
        panic!("expected an error");
    };
    assert!(e.contains("cofre indisponível"));
    assert!(!s.media.saving_key, "the button is usable again");
}

#[test]
fn testing_the_key_reads_the_label_and_the_spend() {
    let dir = tempdir();
    let (mut s, _) = shell(dir.path(), Some(KeySource::Keyring));
    media::test_key(&mut s);
    let k = s.media.check.ready().expect("checked");
    assert_eq!(k.label, "zerith-local");
    assert_eq!(k.usage_usd, Some(1.42));

    s.media.backend.check_key = std::sync::Arc::new(|| Err("chave inválida".into()));
    media::test_key(&mut s);
    assert_eq!(s.media.check, Load::Failed("chave inválida".into()));
}

#[test]
fn listing_models_needs_no_key() {
    let dir = tempdir();
    let (mut s, _) = shell(dir.path(), None);
    s.navigate(Screen::Media);
    assert_eq!(s.media.key_state, Load::Ready(None));
    media::start_search(&mut s, Kind::Video);
    assert_eq!(s.media.video.rows.len(), PAGE, "search works without a key");
}
