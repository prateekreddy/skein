//! What a box puts in its own drop-box cannot make skein read anything else (SKEIN-1406).

use super::*;
use std::os::unix::fs::OpenOptionsExt;

/// The three queues a box writes through a drop-box, each with a record of the shape its reader
/// keeps, so that a file left out below was left out for how it was put there and not for what it
/// says.
const QUEUES: [(&str, &str); 3] = [
    (
        "substrate",
        r#"{"id":"ID","kind":"apt","packages":["tmux"],"asked":"2026-10-30T12:00:00Z"}"#,
    ),
    (
        "gitgate",
        r#"{"id":"ID","repo":"acme/thing","reason":"x","asked":"2026-10-30T12:00:00Z"}"#,
    ),
    (
        "asks",
        r#"{"id":"ID","question":"keep the table?","options":[],"asked":"2026-10-30T12:00:00Z"}"#,
    ),
];

/// A box of `web-main`, inside the real cover, files one ordinary request and plants three things
/// beside it: a link to a file holding a valid record that only the fleet side can read, a FIFO,
/// and a record padded past the drop-box cap. Then the queue's real reader, the one the cockpit's
/// poll calls, reads the fixture fleet, within a time bound.
///
/// **What would make it fail**, each sabotage run on `boxfile::open` or `boxfile::read` and
/// watched failing here for all three queues:
/// * `O_NOFOLLOW` dropped: the planted record is listed as the box's own;
/// * `O_NONBLOCK` dropped: the reader waits on the FIFO and the time bound fails it;
/// * the cap not applied: the padded record is listed;
/// * and, as the regression guard, the listing reading files any other way (the sabotage run was
///   `read_named` looking for `.jsn`): the ordinary request is not listed as it was filed.
fn a_box_cannot_make_the_queue_read_anything_but_its_own_files(queue: &str, shape: &str) {
    if !bwrap_works() {
        return skip(
            "bwrap cannot create a user namespace here, so links, FIFOs and oversized files in a \
             drop-box were NOT exercised",
        );
    }
    let record = |id: &str| shape.replace("ID", id);
    let fleet = Fleet::make_on_volume(&format!("dropbox-{queue}"));
    let volume = fleet.volume.clone().expect("this fleet is on a volume");
    let drop_box = fleet
        .fleet_root
        .join(format!(".skein/{queue}/requests/web-main"));
    // Where the fleet side keeps what no box may read; the cover hides it from the box.
    let planted = fleet.fleet_root.join(".skein/private/planted.json");
    fs::write(&planted, record("planted")).unwrap();
    let big = shape.replace(
        "\"ID\"",
        &format!("\"big\",\"pad\":\"{}\"", "p".repeat(70_000)),
    );

    let said = String::from_utf8_lossy(&fleet.in_box(
        Born::Covered,
        r#"cd "$1" || { echo "no drop-box $1"; exit 0; }
printf '%s\n' "$2" >ok.json && echo "filed ok" || echo "could not file ok"
ln -s "$3" planted.json && echo "planted a link" || echo "could not plant a link"
mkfifo fifo.json && echo "made a fifo" || echo "could not make a fifo"
printf '%s\n' "$4" >big.json && echo "filed big" || echo "could not file big"
cat "$3" >/dev/null 2>&1 && echo "THE BOX CAN READ $3" || echo "the box cannot read $3""#,
        &[
            drop_box.to_string_lossy().into_owned(),
            record("ok"),
            planted.to_string_lossy().into_owned(),
            big,
        ],
    ))
    .to_string();

    // The premises, read from outside: each thing is in the drop-box as what it claims to be, and
    // the link's target is beyond the box, so a listing that showed it read what the box cannot.
    let kind = |n: &str| fs::symlink_metadata(drop_box.join(n)).map(|m| m.file_type());
    assert!(
        kind("planted.json").is_ok_and(|t| t.is_symlink()),
        "the box could not plant a link, so nothing below is tested:\n{said}"
    );
    assert!(
        kind("fifo.json").is_ok_and(|t| {
            use std::os::unix::fs::FileTypeExt;
            t.is_fifo()
        }),
        "the box could not make a FIFO:\n{said}"
    );
    assert!(
        fs::metadata(drop_box.join("big.json")).is_ok_and(|m| m.len() > 64 << 10),
        "the box could not file an oversized request:\n{said}"
    );
    assert!(
        said.contains("the box cannot read"),
        "the planted target is readable from the box, so it proves nothing:\n{said}"
    );

    // The real read, the way the server runs it, with a time bound: a reader parked on the FIFO is
    // released by opening it for writing, and then fails here rather than hanging the suite.
    let _lock = common::env_lock();
    let mut env = common::env_pins();
    env.set("SKEIN_HOME", &volume)
        .set("SKEIN_FLEET_ROOT", &fleet.fleet_root);
    let read = |queue: &str| -> Vec<(String, String)> {
        let (tx, rx) = std::sync::mpsc::channel();
        let q = queue.to_string();
        std::thread::spawn(move || {
            let _real = skein::place::seam::real_crossings();
            let sandbox = skein::place::fleet_sandbox();
            let got: Result<Vec<(String, String)>, String> = match q.as_str() {
                "substrate" => skein::substrate::list(&sandbox)
                    .map(|v| v.into_iter().map(|r| (r.id, r.box_name)).collect()),
                "gitgate" => skein::gitgate::list(&sandbox)
                    .map(|v| v.into_iter().map(|r| (r.id, r.box_name)).collect()),
                _ => skein::asks::list(&sandbox)
                    .map(|v| v.into_iter().map(|r| (r.id, r.box_name)).collect()),
            };
            let _ = tx.send(got);
        });
        match rx.recv_timeout(std::time::Duration::from_secs(20)) {
            Ok(got) => got.unwrap_or_else(|e| panic!("the {queue} queue could not be read: {e}")),
            Err(_) => {
                let _ = fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(drop_box.join("fifo.json"));
                panic!("the {queue} reader waited on a FIFO a box put in its drop-box")
            }
        }
    };
    let shown = read(queue);
    let ids: Vec<&str> = shown.iter().map(|(id, _)| id.as_str()).collect();

    assert!(
        !ids.contains(&"planted"),
        "the {queue} reader followed a link a box planted and listed a record only the fleet side \
         can read: {shown:?}\n{said}"
    );
    assert!(
        !ids.contains(&"big"),
        "the {queue} reader listed a request over the drop-box cap: {shown:?}"
    );
    assert_eq!(
        shown,
        vec![("ok".to_string(), "web-main".to_string())],
        "the {queue} reader no longer lists the box's ordinary request as it was filed:\n{said}"
    );

    // The control: the same bytes the link named, put in the drop-box as a regular file, are
    // listed, so the absence above is the link refused and not a record the reader would drop.
    fs::remove_file(drop_box.join("planted.json")).unwrap();
    fs::write(drop_box.join("planted.json"), fs::read(&planted).unwrap()).unwrap();
    let again = read(queue);
    assert!(
        again.iter().any(|(id, _)| id == "planted"),
        "the planted record is not one the {queue} reader keeps, so its absence proved nothing: \
         {again:?}"
    );
    drop(env);
}

#[test]
fn a_box_cannot_make_the_package_queue_read_anything_but_its_own_files() {
    let (queue, shape) = QUEUES[0];
    a_box_cannot_make_the_queue_read_anything_but_its_own_files(queue, shape);
}

#[test]
fn a_box_cannot_make_the_git_write_queue_read_anything_but_its_own_files() {
    let (queue, shape) = QUEUES[1];
    a_box_cannot_make_the_queue_read_anything_but_its_own_files(queue, shape);
}

#[test]
fn a_box_cannot_make_the_ask_queue_read_anything_but_its_own_files() {
    let (queue, shape) = QUEUES[2];
    a_box_cannot_make_the_queue_read_anything_but_its_own_files(queue, shape);
}
