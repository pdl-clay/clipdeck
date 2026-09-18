//! Subcomandos que abrem o banco direto, sem depender do daemon.

use std::process::Stdio;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde_json::json;

use crate::clipboard;
use crate::config::{self, Config};
use crate::db::{Db, Item, Stats};
use crate::paste::Combo;
use crate::{app, autostart, shortcut, util, watcher};

#[derive(Debug, Parser)]
#[command(
    name = "transferencia",
    version,
    about = "Histórico da área de transferência para COSMIC (Super+V)",
    long_about = None
)]
pub struct Cli {
    /// Sobe só o daemon, sem abrir a janela (usado no autostart)
    #[arg(long)]
    pub daemon: bool,
    /// Alterna o popup (usado pelo atalho Super+V)
    #[arg(long)]
    pub toggle: bool,
    /// Abre o popup
    #[arg(long)]
    pub show: bool,
    /// Fecha o popup
    #[arg(long)]
    pub hide: bool,
    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    /// Ação pedida à instância única.
    pub fn action(&self) -> app::Action {
        if self.daemon {
            app::Action::Daemon
        } else if self.show {
            app::Action::Show
        } else if self.hide {
            app::Action::Hide
        } else {
            app::Action::Toggle
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Lista os itens mais recentes
    List {
        /// Quantidade máxima de itens
        #[arg(short, long, default_value_t = 25)]
        limit: usize,
        /// Saída em JSON
        #[arg(long)]
        json: bool,
    },
    /// Busca no histórico
    Search {
        /// Termo(s) de busca
        query: Vec<String>,
        #[arg(short, long, default_value_t = 25)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Coloca um item do histórico de volta no clipboard
    Copy {
        /// Id do item (veja `list`)
        id: i64,
        /// Para HTML: copia como texto puro em vez de text/html
        #[arg(long)]
        plain: bool,
    },
    /// Fixa um item (não é descartado pelo limite de histórico)
    Pin { id: i64 },
    /// Desfixa um item
    Unpin { id: i64 },
    /// Remove um item do histórico
    Remove { id: i64 },
    /// Apaga o histórico
    Clear {
        /// Mantém os itens fixados
        #[arg(long)]
        keep_pinned: bool,
        /// Confirma a remoção (obrigatório)
        #[arg(long)]
        yes: bool,
    },
    /// Mostra o estado do banco e da configuração
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Instala o autostart e o atalho Super+V, e sobe o daemon
    Install,
    /// Remove o autostart e o atalho Super+V
    Uninstall {
        /// Apaga também o histórico
        #[arg(long)]
        purge: bool,
    },
    /// Envia uma combinação de teclas para a janela em foco (sem mudar o foco)
    Key {
        /// `ctrl+v`, `super+v`, `up`, `down`, `enter`, `escape`, `delete`, `tab`
        combo: String,
    },
    /// Diagnóstico das dependências e da integração com o COSMIC
    Doctor,
}

impl Command {
    pub fn run(self) -> Result<()> {
        let cfg = Config::load();
        match self {
            Command::List { limit, json } => {
                let db = open_db()?;
                print_items(&db.recent(limit)?, json)
            }
            Command::Search { query, limit, json } => {
                let db = open_db()?;
                print_items(&db.search(&query.join(" "), limit)?, json)
            }
            Command::Copy { id, plain } => {
                let db = open_db()?;
                let item = db
                    .get(id)?
                    .with_context(|| format!("item {id} não existe no histórico"))?;
                let payload = clipboard::payload_for_item(&item, plain)?;
                clipboard::set(payload)?;
                println!("item {id} ({}) no clipboard", item.mime);
                Ok(())
            }
            Command::Pin { id } => set_pinned(id, true),
            Command::Unpin { id } => set_pinned(id, false),
            Command::Remove { id } => {
                let mut db = open_db()?;
                if db.remove(id)? {
                    println!("item {id} removido");
                    Ok(())
                } else {
                    bail!("item {id} não existe no histórico")
                }
            }
            Command::Clear { keep_pinned, yes } => {
                if !yes {
                    bail!("nada foi apagado: confirme com --yes");
                }
                let mut db = open_db()?;
                let removed = db.clear(keep_pinned)?;
                println!("{removed} item(ns) removido(s)");
                Ok(())
            }
            Command::Status { json } => {
                let db = open_db()?;
                let stats = db.stats()?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json!({
                            "db": config::db_path(),
                            "items": stats.items,
                            "pinned": stats.pinned,
                            "bytes": stats.bytes,
                            "oldest": stats.oldest,
                            "newest": stats.newest,
                            "wl_copy": clipboard::which("wl-copy").map(|p| p.to_string_lossy().into_owned()),
                            "auto_paste": cfg.auto_paste,
                            "max_items": cfg.max_items,
                            "max_total_bytes": cfg.max_total_bytes,
                        }))?
                    );
                } else {
                    print_status(&stats);
                }
                Ok(())
            }
            Command::Install => install(),
            Command::Uninstall { purge } => uninstall(purge),
            Command::Key { combo } => {
                let combo = Combo::from_str(&combo).map_err(|()| {
                    anyhow::anyhow!(
                        "combinação desconhecida: {combo} (aceito: ctrl+v, super+v, up, down, enter, escape, delete, tab)"
                    )
                })?;
                crate::paste::send_combo(combo)?;
                println!("{combo} enviado para a janela em foco");
                Ok(())
            }
            Command::Doctor => doctor(),
        }
    }
}

fn open_db() -> Result<Db> {
    Db::open(&config::db_path())
}

fn set_pinned(id: i64, pinned: bool) -> Result<()> {
    let mut db = open_db()?;
    if db.set_pinned(id, pinned)? {
        println!("item {id} {}", if pinned { "fixado" } else { "desfixado" });
        Ok(())
    } else {
        bail!("item {id} não existe no histórico")
    }
}

fn print_items(items: &[Item], json: bool) -> Result<()> {
    if json {
        let payload: Vec<_> = items
            .iter()
            .map(|item| {
                json!({
                    "id": item.id,
                    "kind": item.kind.as_str(),
                    "mime": item.mime,
                    "preview": item.preview(120),
                    "bytes": item.bytes,
                    "pinned": item.pinned,
                    "created_at": item.created_at,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }
    if items.is_empty() {
        println!("(histórico vazio)");
        return Ok(());
    }
    for item in items {
        println!(
            "{:>6}  {:<6}  {:>10}  {}{}",
            item.id,
            item.kind.as_str(),
            util::human_age(item.created_at),
            if item.pinned { "★ " } else { "" },
            item.preview(100),
        );
    }
    Ok(())
}

fn print_status(stats: &Stats) {
    println!("banco:  {}", config::db_path().display());
    println!("config: {}", config::config_path().display());
    println!(
        "itens:  {} ({} fixados, {})",
        stats.items,
        stats.pinned,
        util::human_bytes(stats.bytes)
    );
    if let Some(newest) = stats.newest {
        println!("último: {}", util::human_age(newest));
    }
    match clipboard::which("wl-copy") {
        Some(path) => println!("wl-copy: {}", path.display()),
        None => println!("wl-copy: AUSENTE (instale wl-clipboard)"),
    }
}

fn install() -> Result<()> {
    let binary = shortcut::current_binary();
    if !binary.contains('/') {
        bail!("não descobri o caminho do executável; rode o binário instalado (ex.: ~/.local/bin/transferencia install)");
    }

    let outcome = autostart::install(&binary)?;
    println!(
        "autostart: {} ({})",
        outcome.path.display(),
        if outcome.changed {
            "gravado"
        } else {
            "já estava igual"
        }
    );

    match shortcut::install(&binary) {
        Ok(outcome) => {
            if let Some(backup) = outcome.backup {
                println!("backup:    {}", backup.display());
            }
            println!(
                "atalho:    {} ({})",
                outcome.path.display(),
                if outcome.changed {
                    "Super+V adicionado"
                } else {
                    "já estava instalado"
                }
            );
        }
        Err(err) => {
            println!("atalho:    NÃO instalado ({err:#})");
            println!("           adicione à mão em Ajustes → Teclado → Atalhos personalizados:");
            println!("           comando: {binary} --toggle   tecla: Super+V");
        }
    }

    std::process::Command::new(&binary)
        .arg("--daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("iniciando o daemon")?;
    println!("daemon:    início pedido (se já havia uma instância, ela continua)");
    Ok(())
}

fn uninstall(purge: bool) -> Result<()> {
    let binary = shortcut::current_binary();

    let outcome = shortcut::uninstall(&binary)?;
    println!(
        "atalho:    {} ({})",
        outcome.path.display(),
        if outcome.changed {
            "removido"
        } else {
            "não estava instalado"
        }
    );

    let outcome = autostart::uninstall()?;
    println!(
        "autostart: {} ({})",
        outcome.path.display(),
        if outcome.changed {
            "removido"
        } else {
            "não estava instalado"
        }
    );

    if purge {
        let db = config::db_path();
        for path in [db.clone(), db.with_extension("db-wal"), db.with_extension("db-shm")] {
            if path.exists() {
                std::fs::remove_file(&path)
                    .with_context(|| format!("removendo {}", path.display()))?;
            }
        }
        println!("histórico: apagado ({})", db.display());
    } else {
        println!("histórico: preservado em {}", config::db_path().display());
    }
    Ok(())
}

fn doctor() -> Result<()> {
    let cfg = Config::load();
    let binary = shortcut::current_binary();
    println!("binário:   {binary}");
    println!(
        "config:    {} {}",
        config::config_path().display(),
        if config::config_path().is_file() {
            ""
        } else {
            "(ausente: usando padrões)"
        }
    );

    match Db::open(&config::db_path()) {
        Ok(db) => {
            let stats = db.stats()?;
            println!(
                "banco:     {} ({} itens, {}, {} fixados)",
                config::db_path().display(),
                stats.items,
                util::human_bytes(stats.bytes),
                stats.pinned
            );
            println!("integridade: {}", db.integrity_check()?);
            match watcher::connect(cfg.clone(), db) {
                Ok(_) => println!("captura:   ok (data-control disponível)"),
                Err(err) => println!("captura:   INDISPONÍVEL ({err})"),
            }
        }
        Err(err) => println!("banco:     ERRO ({err:#})"),
    }

    match clipboard::which("wl-copy") {
        Some(path) => println!("wl-copy:   {}", path.display()),
        None => println!("wl-copy:   AUSENTE (instale wl-clipboard)"),
    }
    match crate::paste::check_support() {
        Ok(()) => println!("teclado virtual: ok"),
        Err(err) => println!("teclado virtual: INDISPONÍVEL ({err})"),
    }
    println!(
        "autostart: {} ({})",
        autostart::autostart_path().display(),
        if autostart::is_installed() {
            "instalado"
        } else {
            "ausente"
        }
    );
    println!(
        "atalho:    {} ({})",
        shortcut::shortcut_path().display(),
        if shortcut::is_installed(&binary) {
            "Super+V instalado"
        } else {
            "ausente"
        }
    );
    Ok(())
}
