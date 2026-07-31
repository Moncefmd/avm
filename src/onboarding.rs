use crate::cli::Shell;
use crate::completion;
use crate::error::{AvmError, Result};
use crate::profile;
use crate::shell;
use crate::store::Store;

pub(crate) fn init(
    store: &Store,
    selected_shell: Shell,
    install_completion: bool,
    dry_run: bool,
) -> Result<()> {
    let integration = shell::plan_integration(store, selected_shell)?;
    let mut completion = install_completion
        .then(|| completion::plan_install(store, selected_shell, true))
        .transpose()?;

    let mut profile_requests = integration.profile_requests.clone();
    if let Some(plan) = &completion {
        profile_requests.extend(plan.profile_requests.clone());
    }
    let profile_updates = profile::plan_updates(profile_requests)?;

    if dry_run {
        println!("AVM initialization preview for {selected_shell}:");
        shell::preview(&integration);
        if let Some(plan) = &completion {
            completion::preview(plan);
        } else {
            println!("Tab completion: skipped (--no-completion)");
        }
        let completion_flag = if install_completion {
            ""
        } else {
            " --no-completion"
        };
        println!(
            "Run `avm init --shell {selected_shell}{completion_flag}` to apply these changes."
        );
        println!("No files were changed.");
        return Ok(());
    }

    let _integration_lock = store.lock_state(false)?;
    if let Some(plan) = &mut completion {
        plan.apply_file()?;
    }
    shell::apply_integration_locked(store, &integration)?;
    profile::apply_updates(profile_updates)?;

    println!("Initialized AVM for {selected_shell}.");
    println!(
        "Managed argocd command: {}",
        integration.dispatcher.display()
    );
    if let Some(plan) = &completion {
        println!("Tab completion: {}", plan.destination().display());
    } else {
        println!("Tab completion: skipped");
    }
    println!("AVM did not install or change an Argo CD version selection.");
    println!("Open a new terminal to use the managed `argocd` command.");
    println!("If no version is already selected, run `avm default stable`.");
    Ok(())
}

pub(crate) fn uninit(store: &Store, dry_run: bool, remove_path: bool) -> Result<()> {
    let integration_lock = store.lock_existing_state(false)?;
    let (dispatcher, mut dispatcher_warning) = match store.dispatcher_removal_is_safe() {
        Ok(true) if integration_lock.is_none() => (
            false,
            Some("the AVM integration state lock is missing".to_owned()),
        ),
        Ok(removable) => (removable, None),
        Err(error @ AvmError::UnmanagedShim { .. }) => (false, Some(error.to_string())),
        Err(error) => return Err(error),
    };
    let mut profile_requests = shell::cleanup_profile_requests()?;
    profile_requests.extend(completion::activation_removal_requests()?);
    let profile_updates = profile::plan_removals(profile_requests)?;
    let completion = completion::plan_cleanup(store)?;
    let windows_path = shell::plan_windows_path_cleanup(store, remove_path)?;

    profile::preflight_updates(&profile_updates)?;
    completion::preflight_cleanup(&completion)?;
    shell::preflight_windows_path_cleanup(&windows_path)?;

    if dry_run {
        println!("AVM shell-integration cleanup preview:");
        for update in &profile_updates {
            println!(
                "Shell profile: remove AVM-managed blocks from {}",
                update.path().display()
            );
        }
        completion::preview_cleanup(&completion);
        shell::preview_windows_path_cleanup(&windows_path, &store.paths.bin);
        if dispatcher {
            println!(
                "Dispatcher launcher: remove {}",
                store.shim_path().display()
            );
        } else if let Some(warning) = &dispatcher_warning {
            println!(
                "Dispatcher launcher: preserve {} ({warning})",
                store.shim_path().display()
            );
        }
        println!("Installed Argo CD versions, selections, and cache: preserve");
        println!(
            "Run `avm uninit{}` to apply this cleanup.",
            if remove_path { " --remove-path" } else { "" }
        );
        println!("No files were changed.");
        return Ok(());
    }

    let profiles_removed = profile_updates.len();
    profile::apply_updates(profile_updates)?;
    shell::apply_windows_path_cleanup(store, windows_path)?;
    let completions_removed = completion::apply_cleanup(completion)?;
    let dispatcher_removed = if dispatcher {
        match store.remove_dispatcher_locked() {
            Ok(removed) => removed,
            Err(error @ AvmError::UnmanagedShim { .. }) => {
                dispatcher_warning = Some(error.to_string());
                false
            }
            Err(error) => return Err(error),
        }
    } else {
        false
    };
    drop(integration_lock);

    println!("Removed AVM-managed shell integration.");
    println!("Shell profiles updated: {profiles_removed}");
    println!("Completion scripts removed: {completions_removed}");
    println!(
        "Dispatcher launcher removed: {}",
        if dispatcher_removed { "yes" } else { "no" }
    );
    if let Some(warning) = dispatcher_warning {
        eprintln!("warning: preserved dispatcher artifacts: {warning}");
    }
    println!("Installed Argo CD versions, selections, and cache were preserved.");
    Ok(())
}
