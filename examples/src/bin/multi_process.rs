#[cfg(feature = "multiple-process")]
fn main() {
    use std::time::Instant;

    futures::executor::block_on(async {
        let base = std::env::temp_dir().join(format!("pglite-example-mp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);

        let db = pglite::PGlite::open_multi_process(&base, pglite::MultiProcessOptions::default())
            .await
            .expect("open multi-process");

        db.exec("CREATE TABLE accounts (id INT PRIMARY KEY, balance INT NOT NULL)")
            .await
            .expect("create");
        db.exec("INSERT INTO accounts SELECT g, 100 FROM generate_series(1, 4) g")
            .await
            .expect("seed");

        let tx1 = db.transaction().await.expect("tx1");
        tx1.exec("UPDATE accounts SET balance = balance - 30 WHERE id = 1")
            .await
            .expect("debit");

        let rows = db
            .query("SELECT balance FROM accounts WHERE id = 1", &[])
            .await
            .expect("read outside tx");
        println!(
            "other connections still see balance {} while tx1 is open",
            rows[0].get::<i32>(0).expect("balance")
        );

        let tx2 = db.transaction().await.expect("tx2");
        tx2.exec("UPDATE accounts SET balance = balance + 5 WHERE id = 2")
            .await
            .expect("credit");
        tx2.commit().await.expect("commit tx2");
        println!("tx2 committed while tx1 was still in flight");

        tx1.commit().await.expect("commit tx1");

        let started = Instant::now();
        let workers: Vec<_> = (0..4)
            .map(|w| {
                let db = db.clone();
                std::thread::spawn(move || {
                    futures::executor::block_on(async move {
                        for i in 0..25 {
                            db.exec(&format!(
                                "UPDATE accounts SET balance = balance + 1 WHERE id = {}",
                                (w * 25 + i) % 4 + 1
                            ))
                            .await
                            .expect("worker update");
                        }
                    })
                })
            })
            .collect();
        for worker in workers {
            worker.join().expect("join");
        }

        let rows = db
            .query("SELECT sum(balance)::INT FROM accounts", &[])
            .await
            .expect("sum");
        println!(
            "4 threads ran 100 updates across pooled backends in {:?}; total balance {}",
            started.elapsed(),
            rows[0].get::<i32>(0).expect("sum")
        );

        db.close().await.expect("close");
        let _ = std::fs::remove_dir_all(&base);
    });
}

#[cfg(not(feature = "multiple-process"))]
fn main() {
    eprintln!(
        "run with: cargo run -p pglite-examples --features multiple-process --bin multi_process"
    );
}
