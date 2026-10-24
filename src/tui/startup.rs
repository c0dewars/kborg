//! Startup animation and theme preview.
use super::*;

pub(super) fn utf8_locale() -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|value| !value.is_empty())
        .is_some_and(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("utf-8") || value.contains("utf8")
        })
}

/// Plays the startup animation. Returns false when the user pressed Ctrl-C
/// (quit), true otherwise, including any other key that skips it. Only cells
/// that change are redrawn, inside synchronized updates, so it never tears.
pub(super) fn startup_animation(theme: &Theme, color: bool) -> Result<bool> {
    let draw_failed = |_| "startup draw failed";
    let (columns, rows) = terminal::size().map_err(|_| "terminal size failed")?;
    let background = theme.get("background");
    let Some(splash) =
        crate::splash::Splash::new(columns, rows, background, theme.get("text"), utf8_locale())
    else {
        return Ok(true);
    };
    let paint = |rgb: themes::Rgb| if color { rgb.color() } else { Color::Reset };
    let bg = paint(background);
    let mut out = io::stdout();
    queue!(
        out,
        themes::BackgroundColor(bg),
        terminal::Clear(ClearType::All)
    )
    .map_err(draw_failed)?;
    out.flush().map_err(draw_failed)?;
    let started = Instant::now();
    let frame_time = Duration::from_millis(16);
    let mut previous: std::collections::HashMap<(u16, u16), (char, themes::Rgb)> =
        std::collections::HashMap::new();
    loop {
        let t = started.elapsed().as_secs_f32();
        if t >= crate::splash::DURATION {
            break;
        }
        if terminal::size().map_err(|_| "terminal size failed")? != (columns, rows) {
            break;
        }
        let current: std::collections::HashMap<_, _> = splash
            .frame(t)
            .into_iter()
            .map(|cell| ((cell.x, cell.y), (cell.ch, cell.rgb)))
            .collect();
        queue!(out, BeginSynchronizedUpdate).map_err(draw_failed)?;
        for position in previous.keys().filter(|key| !current.contains_key(key)) {
            queue!(
                out,
                cursor::MoveTo(position.0, position.1),
                themes::BackgroundColor(bg),
                style::Print(' ')
            )
            .map_err(draw_failed)?;
        }
        for (position, (ch, rgb)) in &current {
            if previous.get(position) == Some(&(*ch, *rgb)) {
                continue;
            }
            queue!(
                out,
                cursor::MoveTo(position.0, position.1),
                themes::ForegroundColor(paint(*rgb)),
                themes::BackgroundColor(bg),
                style::Print(*ch)
            )
            .map_err(draw_failed)?;
        }
        queue!(out, EndSynchronizedUpdate).map_err(draw_failed)?;
        out.flush().map_err(draw_failed)?;
        previous = current;
        let wait =
            frame_time.saturating_sub(started.elapsed().saturating_sub(Duration::from_secs_f32(t)));
        if event::poll(wait).map_err(|_| "startup input failed")?
            && let Event::Key(key) = event::read().map_err(|_| "startup input failed")?
            && key.kind == KeyEventKind::Press
        {
            return Ok(
                !(key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
            );
        }
    }
    Ok(true)
}

/// Event-loop wake interval: 60 fps while a request is in flight, the view
/// is still loading, or the user typed in the last second; otherwise a calm
/// 100 ms tick. Background data updates alone never raise the rate, so a
/// busy cluster does not keep the loop spinning.
pub(super) fn loop_tick(app: &App, last_activity: Instant) -> Duration {
    let lively =
        app.busy || app.stale || !app.connected || last_activity.elapsed() < Duration::from_secs(1);
    Duration::from_millis(if lively { 16 } else { 100 })
}

/// `kborg startup preview`: play the animation once, even if disabled.
pub fn play_startup_animation(theme_id: Option<&str>, color: bool) -> Result<()> {
    let theme = themes::find(theme_id.unwrap_or(&themes::selected()))?;
    let guard = Terminal::enter(false)?;
    let result = startup_animation(&theme, color);
    drop(guard);
    result.map(|_| ())
}

pub fn preview(theme: Theme, color: bool) -> Result<()> {
    if !io::stdout().is_terminal() {
        println!("{} ({}) | validated palette", theme.name, theme.id);
        return Ok(());
    }
    let mut out = io::stdout();
    let bg = if color {
        theme.get("background").color()
    } else {
        Color::Reset
    };
    for (role, text) in [
        ("text", "Resource: demo/api | 2/2 ready"),
        ("success", "SUCCESS rollout healthy"),
        ("warning", "PRODUCTION / READ ONLY / STALE"),
        ("danger", "ERROR destructive operation requires approval"),
        ("accent", "FOCUS > details | +added -removed"),
    ] {
        queue!(
            out,
            themes::ForegroundColor(if color {
                theme.get(role).color()
            } else {
                Color::Reset
            }),
            themes::BackgroundColor(bg),
            style::Print(format!("{text}\n"))
        )
        .map_err(|_| "preview failed")?;
    }
    execute!(out, style::ResetColor).map_err(|_| "preview failed".into())
}
