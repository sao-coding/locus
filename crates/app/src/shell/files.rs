//! .sql files: open, save, save as, and the folder bound to the Space
//! (the save dialog starts there).
use super::*;

impl Locus {
    pub(crate) fn open_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            for p in paths {
                let p2 = p.clone();
                let read = cx
                    .background_executor()
                    .spawn(async move { std::fs::read(&p2) })
                    .await;
                let _ = this.update_in(cx, |this, window, cx| match read {
                    Ok(bytes) => {
                        // Strip a UTF-8 BOM; other encodings are read as lossy UTF-8.
                        let b = bytes
                            .strip_prefix(&[0xEF, 0xBB, 0xBF][..])
                            .unwrap_or(&bytes);
                        let text = String::from_utf8_lossy(b).into_owned();
                        this.open_text_tab(p.to_string_lossy().into_owned(), text, window, cx);
                    }
                    Err(e) => crate::shell::toast::show(
                        t!(
                            "file.read_error",
                            path = p.display().to_string(),
                            msg = e.to_string()
                        )
                        .to_string(),
                        cx,
                    ),
                });
            }
        })
        .detach();
    }

    pub(crate) fn open_text_tab(
        &mut self,
        path: String,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Already open in this Space: show it.
        if let Some(t) = self
            .space()
            .tabs
            .iter()
            .find(|t| t.read(cx).doc.path.as_deref() == Some(path.as_str()))
            .cloned()
        {
            let id = t.read(cx).id.clone();
            self.show_tab(&id, window, cx);
            return;
        }
        let conn = self
            .active_tab(cx)
            .read(cx)
            .conn_id
            .clone()
            .or_else(|| self.space().connections.first().cloned());
        let mut data = self.blank_tab(conn, cx);
        data.sql = text;
        data.path = Some(path);
        let tab = self.make_tab(&data, window, cx);
        let s = self.space_mut();
        s.tabs.push(tab);
        s.show_alone(&data.id, cx);
        self.sync_space_conns(self.active_space, cx);
        self.focus_editor(window, cx);
        self.schedule_save(cx);
        cx.notify();
    }

    /// Writes the tab to its file (asks for a path when it has none or for
    /// Save As). With `close`, the tab closes after a successful write.
    pub(crate) fn save_tab(
        &mut self,
        tab: Entity<QueryTab>,
        save_as: bool,
        close: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if tab.read(cx).diagram.is_some() || tab.read(cx).structure.is_some() {
            return;
        }
        let existing = tab.read(cx).doc.path.clone().filter(|_| !save_as);
        let text = tab
            .read(cx)
            .editor
            .read(cx)
            .state
            .read(cx)
            .value()
            .to_string();
        // A new file defaults to the folder bound to the Space (save dialog default).
        let folder = self
            .space()
            .folder
            .clone()
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_dir());
        let ask = existing.is_none().then(|| {
            let dir = tab
                .read(cx)
                .doc
                .path
                .as_ref()
                .and_then(|p| std::path::Path::new(p).parent().map(|d| d.to_path_buf()))
                .or(folder)
                .or_else(|| {
                    std::env::var_os("USERPROFILE")
                        .map(|h| std::path::PathBuf::from(h).join("Documents"))
                })
                .unwrap_or_default();
            let name = format!(
                "{}.sql",
                tab.read(cx)
                    .display_title(cx)
                    .chars()
                    .filter(|c| c.is_alphanumeric() || *c == '_' || *c == ' ')
                    .collect::<String>()
                    .trim()
            );
            let name = if name == ".sql" {
                "query.sql".to_string()
            } else {
                name
            };
            cx.prompt_for_new_path(&dir, Some(&name))
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = match (existing, ask) {
                (Some(p), _) => std::path::PathBuf::from(p),
                (None, Some(rx)) => match rx.await {
                    Ok(Ok(Some(p))) => p,
                    _ => return,
                },
                _ => return,
            };
            let p2 = path.clone();
            let res = cx
                .background_executor()
                .spawn(async move { locus_core::settings::write_file(&p2, text.as_bytes()) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| match res {
                Ok(()) => {
                    let id = tab.update(cx, |t, cx| {
                        t.doc.path = Some(path.to_string_lossy().into_owned());
                        t.doc.modified = false;
                        t.doc.baseline = t.editor.read(cx).state.read(cx).value().to_string();
                        cx.notify();
                        t.id.clone()
                    });
                    this.git_file_saved(cx);
                    this.schedule_save(cx);
                    if close {
                        this.close_tab_now(&id, window, cx);
                    }
                    cx.notify();
                }
                Err(e) => crate::shell::toast::show(
                    t!(
                        "file.write_error",
                        path = path.display().to_string(),
                        msg = e.to_string()
                    )
                    .to_string(),
                    cx,
                ),
            });
        })
        .detach();
    }

    /// Palette "Bind folder…": picks a folder for the active Space.
    pub(crate) fn bind_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(p) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.set_folder(Some(p.to_string_lossy().into_owned()), cx)
            });
        })
        .detach();
    }

    /// Binds (or, with `None`, unbinds) the active Space's folder.
    pub(crate) fn set_folder(&mut self, folder: Option<String>, cx: &mut Context<Self>) {
        self.space_mut().folder = folder;
        self.sync_git(cx);
        self.schedule_save(cx);
        cx.notify();
    }
}
