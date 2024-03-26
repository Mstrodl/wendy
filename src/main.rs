//! penrose :: minimal configuration
//!
//! This file will give you a functional if incredibly minimal window manager that
//! has multiple workspaces and simple client / workspace movement.
use penrose::{
    builtin::{
        actions::{exit, key_handler, modify_with, send_layout_message, spawn},
        layout::{
            messages::{ExpandMain, IncMain, ShrinkMain},
            Monocle,
        },
    },
    core::{
        bindings::{parse_keybindings_with_xmodmap, KeyEventHandler},
        layout::LayoutStack,
        Config, State, WindowManager,
    },
    extensions::hooks::add_ewmh_hooks,
    map,
    pure::Screen,
    util,
    x::{
        atom::Atom,
        property::Prop,
        query::{AppName, Query},
        XConn, XConnExt,
    },
    x11rb::RustConn,
    Result, Xid,
};
use std::collections::{HashMap, HashSet, VecDeque};
use tracing_subscriber::{self, prelude::*};

#[derive(Debug)]
struct PinnedApp<X: XConn> {
    command: &'static str,
    query: Box<dyn Query<X>>,
}

fn get_app_name<X: XConn>(client: Xid, x: &X) -> Option<String> {
    match x.get_prop(client, Atom::WmClass.as_ref()).ok().flatten() {
        Some(Prop::UTF8String(classes)) if !classes.is_empty() => Some(classes[0].clone()),
        _ => None,
    }
}

fn get_pinned_apps<X: XConn>() -> HashMap<&'static str, PinnedApp<X>> {
    HashMap::from([
        (
            "1",
            PinnedApp {
                command: "emacs",
                query: Box::new(AppName("emacs")),
            },
        ),
        (
            "2",
            PinnedApp {
                command: "alacritty",
                query: Box::new(AppName("Alacritty")),
            },
        ),
        (
            "3",
            PinnedApp {
                command: "chromium",
                query: Box::new(AppName("chromium")),
            },
        ),
        (
            "4",
            PinnedApp {
                command: "DiscordCanary",
                query: Box::new(AppName("DiscordCanary")),
            },
        ),
        (
            "5",
            PinnedApp {
                command: "slack",
                query: Box::new(AppName("slack")),
            },
        ),
    ])
}

const TAGS: [&'static str; 10] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "10"];

fn raw_key_bindings() -> HashMap<String, Box<dyn KeyEventHandler<RustConn>>> {
    let mut raw_bindings = map! {
        map_keys: |k: &str| k.to_string();

        "M-j" => modify_with(|cs| cs.focus_down()),
        "M-k" => modify_with(|cs| cs.focus_up()),
        "M-S-j" => modify_with(|cs| cs.swap_down()),
        "M-S-k" => modify_with(|cs| cs.swap_up()),
        "M-S-q" => modify_with(|cs| cs.kill_focused()),
        "M-Tab" => modify_with(|cs| cs.toggle_tag()),
        "M-bracketright" => modify_with(|cs| cs.next_screen()),
        "M-bracketleft" => modify_with(|cs| cs.previous_screen()),
        "M-grave" => modify_with(|cs| cs.next_layout()),
        "M-S-grave" => modify_with(|cs| cs.previous_layout()),
        "M-S-Up" => send_layout_message(|| IncMain(1)),
        "M-S-Down" => send_layout_message(|| IncMain(-1)),
        "M-S-Right" => send_layout_message(|| ExpandMain),
        "M-S-Left" => send_layout_message(|| ShrinkMain),
        "M-space" => spawn("dmenu_run"),
        "M-Return" => spawn("alacritty"),
        "M-A-Escape" => exit(),
    };

    for tag in &TAGS {
        raw_bindings.extend([
            (
                format!("M-{}", if tag == &"10" { "0" } else { tag }),
                key_handler(move |state, x: &RustConn| {
                    let apps = get_pinned_apps();
                    if let Some(app) = apps.get(tag) {
                        if !state
                            .client_set
                            .clients()
                            .any(|client| app.query.run(*client, x).unwrap_or(false))
                        {
                            // No client found for this App
                            util::spawn(app.command)?;
                            // (No need to refresh because we're not launched yet)
                            return Ok(());
                        }
                    }
                    if &state.client_set.current_tag() == tag {
                        // Already focused, cycle through them.
                        cycle_workspace(state, tag)?;
                    } else {
                        state.client_set.focus_tag(tag);
                    }
                    x.refresh(state)
                }),
            ),
            // (
            //     format!("M-S-{tag}"),
            //     modify_with(move |client_set| client_set.move_focused_to_tag(tag)),
            // ),
        ]);
    }

    raw_bindings
}

#[derive(Debug, Default)]
struct RecentClients {
    recent_clients: Vec<Xid>,
    chronological_clients: Vec<Xid>,
    gui_open: HashMap<String, bool>,
}

#[derive(Debug, Clone)]
enum Direction {
    Forward,
    Backward,
}

fn cycle_workspace<X: XConn + 'static>(state: &mut State<X>, tag: &str) -> Result<()> {
    let workspace = match state.client_set.workspace(tag) {
        Some(workspace) => workspace,
        None => return Ok(()),
    };
    let focus = workspace.focus().cloned();
    let recent_clients = state.extension_or_default::<RecentClients>();
    let recent_clients = recent_clients.borrow();

    let clients_on_workspace = state
        .client_set
        .workspace(tag)
        .unwrap()
        .clients()
        .collect::<HashSet<_>>();
    let clients_on_workspace = recent_clients
        .chronological_clients
        .iter()
        .filter(|client| clients_on_workspace.contains(client))
        .cloned()
        .collect::<Vec<_>>();
    // Shouldn't really happen, but whatever
    if clients_on_workspace.is_empty() {
        return Ok(());
    }

    let focused_position = focus
        .and_then(|focus| {
            clients_on_workspace
                .iter()
                .cloned()
                .position(|ws| ws == focus)
        })
        .unwrap_or(0);

    let new_focused_position = (focused_position + 1) % clients_on_workspace.len();
    println!("New focused position: {new_focused_position} (was: {focused_position})");
    state
        .client_set
        .focus_client(&clients_on_workspace[new_focused_position]);
    Ok(())
}

fn move_pinned_windows<X: XConn + 'static>(client: Xid, state: &mut State<X>, x: &X) -> Result<()> {
    println!("New window just dropped: {:?}", get_app_name(client, x));
    let tag = get_tag_for_client(client, state, x)?;
    println!("...Tag is {tag}");
    state.client_set.move_client_to_tag(&client, &tag);
    state.client_set.focus_tag(&tag);
    state.client_set.focus_client(&client);

    Ok(())
}

fn populate_new_window<X: XConn + 'static>(client: Xid, state: &mut State<X>, x: &X) -> Result<()> {
    let recent_clients = state.extension_or_default::<RecentClients>();
    let mut recent_clients = recent_clients.borrow_mut();
    recent_clients.recent_clients.insert(0, client);
    recent_clients.chronological_clients.push(client);

    Ok(())
}

fn get_tag_for_client<X: XConn + 'static>(
    client: Xid,
    state: &mut State<X>,
    x: &X,
) -> Result<String> {
    let pinned_apps = get_pinned_apps();
    if let Some((tag, _)) = pinned_apps
        .iter()
        .find(|(_, app)| app.query.run(client, x).unwrap_or(false))
    {
        println!("Belongs to a pinned app :)");
        return Ok(tag.to_string());
    }
    if let Some(app_name) = get_app_name(client, x) {
        if let Some(workspace) = state.client_set.ordered_workspaces().find(|ws| {
            ws.clients().any(|existing_client| {
                get_app_name(*existing_client, x)
                    .map(|new| app_name == new)
                    .unwrap_or(false)
                    && client != *existing_client
            })
        }) {
            println!("App is already open on another workspace");
            return Ok(workspace.tag().to_string());
        }
    }

    if let Some(ws) = state
        .client_set
        .ordered_workspaces()
        .find(|ws| !pinned_apps.contains_key(ws.tag()) && ws.is_empty())
    {
        println!("Empty workspace");
        return Ok(ws.tag().to_string());
    }

    // Create new if we can't find any other groups:
    let last_ws_tag = state
        .client_set
        .ordered_workspaces()
        .filter_map(|ws| ws.tag().parse::<i32>().ok())
        .last()
        .unwrap_or(0);

    let new_tag = (last_ws_tag + 1).to_string();
    create_tag(state, &new_tag)?;

    println!("New tag");
    Ok(new_tag)
}

fn default_layout_factory() -> LayoutStack {
    LayoutStack::new(VecDeque::default(), Monocle::boxed(), VecDeque::default())
}

fn create_tag<X: XConn + 'static>(state: &mut State<X>, tag: &str) -> Result<()> {
    state
        .client_set
        .add_workspace(tag, default_layout_factory())
}

fn backfill_gaps<X: XConn + 'static>(state: &mut State<X>, x: &X) -> Result<()> {
    let pinned_apps = get_pinned_apps::<X>();
    let all_workspaces = state
        .client_set
        .ordered_workspaces()
        .map(|ws| ws.tag().to_string())
        .filter(|tag| !pinned_apps.contains_key(tag.as_str()))
        .collect::<Vec<_>>();

    let screens = state
        .client_set
        .screens()
        .cloned()
        .collect::<Vec<Screen<_>>>();
    let non_empty_workspaces = state
        .client_set
        .ordered_workspaces()
        .filter(|ws| !pinned_apps.contains_key(ws.tag()) && !ws.is_empty())
        .map(|ws| ws.tag().to_string())
        .collect::<Vec<_>>();

    let current_screen_index = state.client_set.current_screen().index();
    let current_screen_workspace_tag = state
        .client_set
        .current_screen()
        .workspace
        .tag()
        .to_string();
    for (index, old_tag) in non_empty_workspaces.iter().enumerate() {
        // All workspaces
        let new_tag = &all_workspaces[index];
        if new_tag != old_tag {
            println!("Moving {old_tag} windows -> {new_tag}");
            let old_workspace = state.client_set.workspace_mut(old_tag).unwrap();
            let old_layouts = old_workspace.set_available_layouts(LayoutStack::default());
            let old_layout = old_workspace.layout_name();
            let old_workspace_clients = old_workspace.clients().cloned().collect::<Vec<_>>();
            let screen = screens
                .iter()
                .find(|screen| screen.workspace.id() == old_workspace.id())
                .map(|screen| (screen.index(), screen.workspace.tag()));
            let focused = old_workspace.focus().cloned();
            for client in old_workspace_clients.iter() {
                state.client_set.move_client_to_tag(client, new_tag);
            }

            let new_workspace = state.client_set.workspace_mut(new_tag).unwrap();
            new_workspace.set_available_layouts(old_layouts);
            new_workspace.set_layout_by_name(&old_layout);
            if let Some((screen, screen_tag)) = screen {
                state.client_set.focus_screen(screen);
                state.client_set.pull_tag_to_screen(new_tag);
                state.client_set.focus_tag(screen_tag);
                if let Some(focused) = focused {
                    state.client_set.focus_client(&focused);
                }
                state.client_set.focus_screen(current_screen_index);
                state.client_set.focus_tag(&current_screen_workspace_tag);
            }
        }
    }
    Ok(())
}

fn populate_windows<X: XConn + 'static>(state: &mut State<X>, x: &X) -> Result<()> {
    let all_clients = state.client_set.clients().cloned().collect::<HashSet<_>>();
    let recent_clients = state.extension_or_default::<RecentClients>();
    let mut recent_clients = recent_clients.borrow_mut();
    recent_clients.recent_clients = recent_clients
        .recent_clients
        .iter()
        .filter(|client| all_clients.contains(client))
        .cloned()
        .collect::<Vec<_>>();
    recent_clients.chronological_clients = recent_clients
        .chronological_clients
        .iter()
        .filter(|client| all_clients.contains(client))
        .cloned()
        .collect::<Vec<_>>();
    let known_clients = recent_clients
        .recent_clients
        .iter()
        .cloned()
        .collect::<HashSet<_>>();
    let mut unknown_clients = all_clients
        .into_iter()
        .filter(|client| !known_clients.contains(client))
        .collect::<Vec<_>>();
    if !unknown_clients.is_empty() {
        recent_clients
            .recent_clients
            .append(&mut unknown_clients.clone());
        recent_clients
            .chronological_clients
            .append(&mut unknown_clients);
    }

    if let Some(current_client) = state.client_set.current_client() {
        if let Some(index) = recent_clients
            .recent_clients
            .iter()
            .position(|client| current_client == client)
        {
            recent_clients.recent_clients.remove(index);
        }
        recent_clients.recent_clients.insert(0, *current_client);
    }

    Ok(())
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("info")
        .finish()
        .init();

    let conn = RustConn::new()?;
    let key_bindings = parse_keybindings_with_xmodmap(raw_key_bindings())?;
    let mut config = add_ewmh_hooks(Config::default());
    config.tags = TAGS.into_iter().map(String::from).collect();
    config.default_layouts = default_layout_factory();
    config.compose_or_set_manage_hook(move_pinned_windows);
    config.compose_or_set_manage_hook(populate_new_window);
    config.compose_or_set_refresh_hook(backfill_gaps);
    config.compose_or_set_refresh_hook(populate_windows);
    let wm = WindowManager::new(config, key_bindings, HashMap::new(), conn)?;

    wm.run()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_parse_correctly_with_xmodmap() {
        let res = parse_keybindings_with_xmodmap(raw_key_bindings());

        if let Err(e) = res {
            panic!("{e}");
        }
    }
}
