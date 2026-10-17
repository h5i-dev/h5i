use h5i_app_gpu::{
    linkding::{Query, Rows, benchmark_fixture, fixture},
    linkding_original as original,
};
use h5i_app_gpu_backend_nvvm::{Failure, Nvvm};
use std::time::Instant;
fn check(gpu: &mut Nvvm) -> Result<(), Failure> {
    let queries = [
        "",
        "alpha",
        "ALPHA",
        "#rust",
        "!unread",
        "!untagged",
        "!unknown",
        "alpha and beta",
        "alpha or not beta",
        "(alpha or #rust) and not !unread",
        "\"beta gamma\"",
        "'saved page'",
        "alpha beta",
        "not not alpha",
        "alpha and",
        "(",
        "#ALPHA",
        "theta",
        "alpha\\x",
    ];
    let mut r = Rows::default();
    let mut bookmarks = vec![];
    let mut seed = 123u64;
    for i in 0..257 {
        let mut b = fixture(i);
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        if i % 4 == 0 {
            b.title = (0..(seed as usize % 129))
                .map(|j| b'a' + ((seed >> (j % 32)) % 26) as u8)
                .collect();
        }
        if i % 7 == 0 {
            b.notes = vec![0, 65, 0, 66];
        }
        b.unread = seed & 1 != 0;
        b.untagged = seed & 2 != 0;
        b.tag_names = (0..(seed as usize % 9))
            .map(|j| format!("tag{}", (seed as usize + j) % 11).into_bytes())
            .collect();
        if i % 5 == 0 {
            b.description = b"alpha beta".to_vec();
        }
        if i % 6 == 0 {
            b.url = b"https://ALPHA.invalid".to_vec();
        }
        r.push(&b);
        bookmarks.push(b);
    }
    let mut random_queries = Vec::new();
    let leaves = [
        "alpha",
        "beta",
        "#tag1",
        "#TAG4",
        "!unread",
        "!untagged",
        "'beta gamma'",
        "\"saved page\"",
        "!unknown",
        "theta",
    ];
    for _ in 0..100 {
        let mut parts = Vec::new();
        for _ in 0..8 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            parts.push(format!(
                "{}{}",
                if seed & 1 != 0 { "not " } else { "" },
                leaves[(seed >> 8) as usize % leaves.len()]
            ));
        }
        random_queries.push(format!(
            "({} or {}) and ({} or {}) or not ({} and {}) and ({} or {})",
            parts[0], parts[1], parts[2], parts[3], parts[4], parts[5], parts[6], parts[7]
        ));
    }
    for lax in [false, true] {
        for s in queries
            .iter()
            .map(|s| s.to_string())
            .chain(random_queries.iter().cloned())
            .chain((0..64).map(|i| {
                format!(
                    "({} or #{}) and not ({} and !{})",
                    queries[1 + i % 3],
                    if i % 2 == 0 { "rust" } else { "missing" },
                    if i % 3 == 0 { "beta" } else { "theta" },
                    if i % 2 == 0 { "unread" } else { "untagged" }
                )
            }))
        {
            let q = Query::new(&s, lax);
            let p = original::parse_search(&s.as_bytes().to_vec());
            let cpu = r.cpu(&q, 1);
            for (i, b) in bookmarks.iter().enumerate() {
                assert_eq!(
                    cpu[i],
                    original::bookmark_matches(&p, lax, b) as u32,
                    "oracle {s} row {i}"
                );
            }
            let uploaded = gpu.upload_linkding(&r)?;
            let out = gpu.linkding(&uploaded, &q)?;
            assert_eq!(out.values, cpu, "GPU {s}");
            assert!(out.errors.iter().all(|e| *e == 0));
        }
    }
    // Field boundary, exact-width, embedded zero, over-width/tag/node capacities.
    let mut edge = Rows::default();
    for n in [0, 127, 128, 129] {
        let mut b = fixture(1);
        b.title = vec![b'a'; n];
        b.description = b"lpha".to_vec();
        b.notes.clear();
        b.url.clear();
        b.tag_names.clear();
        edge.push(&b);
    }
    let mut b = fixture(1);
    b.tag_names = vec![b"x".to_vec(); 9];
    edge.push(&b);
    for q in [
        Query::new("alpha", false),
        Query::new(&vec!["a"; 130].join(" and "), false),
    ] {
        let cpu = edge.cpu(&q, 1);
        let out = gpu.linkding(&gpu.upload_linkding(&edge)?, &q)?;
        for i in 0..cpu.len() {
            assert_eq!(out.errors[i], (cpu[i] == 2) as u32);
            assert_eq!(out.values[i], if cpu[i] == 2 { 0 } else { cpu[i] });
        }
    }
    // Max-node boundary, malformed serialized offsets/indices and truncated SoA.
    for mut q in [
        Query::new(&("not ".to_owned() + &vec!["a"; 128].join(" and ")), false),
        Query::new("alpha", false),
    ] {
        for mode in 0..3 {
            if mode == 1 {
                q.nodes[1] = u32::MAX;
            }
            if mode == 2 {
                q.nodes[0] = 4;
                q.nodes[3] = u32::MAX;
            }
            let cpu = r.cpu(&q, 1);
            let out = gpu.linkding(&gpu.upload_linkding(&r)?, &q)?;
            for i in 0..cpu.len() {
                assert_eq!(out.errors[i], (cpu[i] == 2) as u32);
                assert_eq!(out.values[i], if cpu[i] == 2 { 0 } else { cpu[i] });
            }
        }
    }
    let mut broken = r.clone();
    broken.fields.truncate(512);
    let q = Query::new("alpha", false);
    let out = gpu.linkding(&gpu.upload_linkding(&broken)?, &q)?;
    for i in 0..broken.len() {
        assert_eq!(out.errors[i], (broken.at(&q, i) == 2) as u32);
    }
    let oversized = Rows {
        meta: vec![0; 1_000_001 * 3],
        ..Rows::default()
    };
    let out = gpu.linkding(&gpu.upload_linkding(&oversized)?, &q)?;
    assert!(out.errors.iter().all(|e| *e == 1));
    let q = Query::new("alpha", true);
    let whole = gpu.linkding(&gpu.upload_linkding(&r)?, &q)?;
    let mut joined = vec![];
    for range in [0..128, 128..257] {
        let mut part = Rows::default();
        for i in range {
            part.push(&bookmarks[i]);
        }
        joined.extend(gpu.linkding(&gpu.upload_linkding(&part)?, &q)?.values);
    }
    assert_eq!(whole.values, joined);
    println!(
        "# oracle/CPU/GPU differential passed: queries, lax, fields, capacities, batch independence"
    );
    Ok(())
}
fn main() -> Result<(), Failure> {
    let ptx = std::fs::read_to_string(std::env::args().nth(1).expect("PTX"))?;
    let mut gpu = Nvvm::new(&ptx)?;
    check(&mut gpu)?;
    if std::env::args().any(|s| s == "--check-only") {
        return Ok(());
    }
    println!(
        "query,rows,repeat,upload_ms,kernel_ms,resident_ms,upload_each_ms,cpu1_ms,cpu16_ms,rows_per_second"
    );
    for n in [10_000, 100_000, 1_000_000] {
        let mut rows = Rows::default();
        for i in 1..=n {
            rows.push(&benchmark_fixture(i));
        }
        let start = Instant::now();
        let resident = gpu.upload_linkding(&rows)?;
        let upload = start.elapsed().as_secs_f64() * 1000.;
        for (name, s) in [
            ("alpha", "alpha"),
            (
                "heavy",
                "(alpha or beta) and not (missing or absent) and (gamma or delta) and not (unknown and zeta)",
            ),
        ] {
            let q = Query::new(s, false);
            let expected = rows.cpu(&q, 1);
            gpu.linkding(&resident, &q)?;
            for rep in 0..3 {
                let t = Instant::now();
                let cpu = rows.cpu(&q, 1);
                let cpu1 = t.elapsed().as_secs_f64() * 1000.;
                assert_eq!(cpu, expected);
                let t = Instant::now();
                assert_eq!(rows.cpu(&q, 16), expected);
                let cpu16 = t.elapsed().as_secs_f64() * 1000.;
                let out = gpu.linkding(&resident, &q)?;
                assert_eq!(out.values, expected);
                assert!(out.errors.iter().all(|e| *e == 0));
                let kernel = gpu.kernel_ms;
                let total = gpu.total_ms;
                let t = Instant::now();
                let fresh = gpu.upload_linkding(&rows)?;
                assert_eq!(gpu.linkding(&fresh, &q)?.values, expected);
                let each = t.elapsed().as_secs_f64() * 1000.;
                println!(
                    "{name},{n},{rep},{upload:.6},{kernel:.6},{total:.6},{each:.6},{cpu1:.6},{cpu16:.6},{:.0}",
                    n as f64 * 1000. / total
                );
            }
        }
    }
    Ok(())
}
