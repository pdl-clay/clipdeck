//! Subcomandos que abrem o banco direto, sem depender do daemon.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde_json::json;

use crate::clipboard;
use crate::config::{self, Config};
use crate::db::{Db, Item, Stats};
use crate::paste::Combo;
use crate::{app, autostart, selection, shortcut, util, watcher};

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
    /// Publica a seleção de um item e fica servindo até outro cliente copiar
    #[command(hide = true)]
    Serve {
        id: i64,
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
        /// `ctrl+v`, `super+v`, `up`, `down`, `pageup`, `pagedown`, `enter`, `escape`, `delete`, `tab`
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
            Command::Copy { id, plain } => copy(id, plain),
            Command::Serve { id, plain } => serve(id, plain),
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

/// Publica um item no clipboard por meio de um processo filho: a seleção morre
/// junto com quem a serve, e a CLI precisa sair.
fn copy(id: i64, plain: bool) -> Result<()> {
    let binary = std::env::current_exe().context("descobrindo o executável")?;
    let mut command = std::process::Command::new(binary);
    command.arg("serve").arg(id.to_string());
    if plain {
        command.arg("--plain");
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        // Grupo próprio: o filho sobrevive à CLI e não morre no Ctrl+C do terminal.
        .process_group(0)
        .spawn()
        .context("iniciando quem serve a seleção")?;

    let mut ready = String::new();
    if let Some(stdout) = child.stdout.take() {
        BufReader::new(stdout)
            .read_line(&mut ready)
            .context("esperando a seleção ficar pronta")?;
    }

    let report = ready.trim();
    match report.strip_prefix("ok ") {
        Some(label) => {
            println!("item {id} ({label}) no clipboard");
            Ok(())
        }
        None => {
            let _ = child.wait();
            if report.is_empty() {
                bail!("quem serve a seleção saiu sem publicá-la (erro acima)");
            }
            bail!("{report}");
        }
    }
}

/// Publica a seleção e fica servindo até outro cliente copiar — é o processo
/// que o `copy` deixa rodando em segundo plano.
fn serve(id: i64, plain: bool) -> Result<()> {
    let db = open_db()?;
    let item = db
        .get(id)?
        .with_context(|| format!("item {id} não existe no histórico"))?;
    let payload = clipboard::Payload::for_item(&item, plain)?;
    drop(item);
    drop(db);

    let label = payload.label().to_string();
    let server = selection::Server::start(payload)?;
    // Quem pediu a cópia espera esta linha para saber que a seleção já é nossa.
    println!("ok {label}");
    std::io::stdout().flush().context("avisando quem pediu a cópia")?;
    server.run()
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

    match selection::check_support() {
        Ok(()) => println!("seleção:   ok (o próprio transferencia serve o clipboard)"),
        Err(err) => println!("seleção:   INDISPONÍVEL ({err})"),
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
