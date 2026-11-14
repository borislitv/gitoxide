use crate::Result;
use crate::hex_to_id;
use crate::util::{commit_graph, fixture, fixture_odb};
use gix_hash::{ObjectId, oid};
use gix_object::bstr::ByteSlice;
use gix_traverse::commit::{Parents, topo};
use std::path::PathBuf;

fn odb() -> Result<gix_odb::Handle> {
    fixture_odb("make_repo_for_topo.sh")
}

fn fixture_dir() -> Result<PathBuf> {
    fixture("make_repo_for_topo.sh")
}

/// Run a topo traversal with both commit-graph enabled and disabled to ensure consistency.
fn traverse_both(
    tips: impl IntoIterator<Item = ObjectId> + Clone,
    ends: impl IntoIterator<Item = ObjectId> + Clone,
    odb: &gix_odb::Handle,
    sorting: topo::Sorting,
    parents: Parents,
) -> Result<Vec<ObjectId>> {
    // Without commit graph
    let without_graph: Vec<_> = topo::Builder::from_iters(odb, tips.clone(), Some(ends.clone()))
        .sorting(sorting)
        .with_commit_graph(None)
        .parents(parents)
        .build()?
        .map(|res| res.map(|info| info.id))
        .collect::<std::result::Result<Vec<_>, _>>()?;

    // With commit graph
    let graph = commit_graph(odb.store_ref());
    let with_graph: Vec<_> = topo::Builder::from_iters(odb, tips, Some(ends))
        .sorting(sorting)
        .with_commit_graph(graph)
        .parents(parents)
        .build()?
        .map(|res| res.map(|info| info.id))
        .collect::<std::result::Result<Vec<_>, _>>()?;

    assert_eq!(
        without_graph, with_graph,
        "results must be consistent with and without commit-graph"
    );
    Ok(with_graph)
}

/// Run a topo traversal with a predicate filter.
fn traverse_with_predicate(
    tips: impl IntoIterator<Item = ObjectId> + Clone,
    ends: impl IntoIterator<Item = ObjectId> + Clone,
    odb: &gix_odb::Handle,
    sorting: topo::Sorting,
    parents: Parents,
    predicate: impl FnMut(&oid) -> bool + Clone,
) -> Result<Vec<ObjectId>> {
    // Without commit graph
    let without_graph: Vec<_> = topo::Builder::from_iters(odb, tips.clone(), Some(ends.clone()))
        .sorting(sorting)
        .with_commit_graph(None)
        .parents(parents)
        .with_predicate(predicate.clone())
        .build()?
        .map(|res| res.map(|info| info.id))
        .collect::<std::result::Result<Vec<_>, _>>()?;

    // With commit graph
    let graph = commit_graph(odb.store_ref());
    let with_graph: Vec<_> = topo::Builder::from_iters(odb, tips, Some(ends))
        .sorting(sorting)
        .with_commit_graph(graph)
        .parents(parents)
        .with_predicate(predicate)
        .build()?
        .map(|res| res.map(|info| info.id))
        .collect::<std::result::Result<Vec<_>, _>>()?;

    assert_eq!(
        without_graph, with_graph,
        "results must be consistent with and without commit-graph"
    );
    Ok(with_graph)
}

/// Read baseline file and parse expected commit hashes.
fn read_baseline(fixture_dir: &std::path::Path, name: &str) -> Result<Vec<String>> {
    let buf = std::fs::read(fixture_dir.join(format!("{name}.baseline")))?;
    Ok(buf.lines().map(|s| s.to_str().unwrap().to_string()).collect())
}

mod basic {
    use super::*;

    #[test]
    fn simple() -> gix_testtools::TestResult {
        let odb = odb()?;
        let tip = hex_to_id("62ed296d9986f50477e9f7b7e81cd0258939a43d");

        let expected = [
            "62ed296d9986f50477e9f7b7e81cd0258939a43d",
            "722bf6b8c3d9e3a11fa5100a02ed9b140e1d209c",
            "3be0c4c793c634c8fd95054345d4935d10a0879a",
            "2083b02a78e88b747e305b6ed3d5a861cf9fb73f",
            "302a5d0530ec688c241f32c2f2b61b964dd17bee",
            "d09384f312b03e4a1413160739805ff25e8fe99d",
            "22fbc169eeca3c9678fc7028aa80fad5ef49019f",
            "eeab3243aad67bc838fc4425f759453bf0b47785",
            "693c775700cf90bd158ee6e7f14dd1b7bd83a4ce",
            "33eb18340e4eaae3e3dcf80222b02f161cd3f966",
            "1a27cb1a26c9faed9f0d1975326fe51123ab01ed",
            "f1cce1b5c7efcdfa106e95caa6c45a2cae48a481",
            "945d8a360915631ad545e0cf04630d86d3d4eaa1",
            "a863c02247a6c5ba32dff5224459f52aa7f77f7b",
            "2f291881edfb0597493a52d26ea09dd7340ce507",
            "9c46b8765703273feb10a2ebd810e70b8e2ca44a",
            "fb3e21cf45b04b617011d2b30973f3e5ce60d0cd",
        ]
        .map(hex_to_id);

        let result = traverse_both([tip], [], &odb, topo::Sorting::TopoOrder, Parents::All)?;
        assert_eq!(result, expected);

        // Verify against baseline
        let baseline = read_baseline(&fixture_dir()?, "all-commits")?;
        let expected_strs: Vec<_> = expected.iter().map(std::string::ToString::to_string).collect();
        assert_eq!(expected_strs, baseline, "Baseline must match the expectation");

        Ok(())
    }

    #[test]
    fn duplicate_tips_are_ignored() -> gix_testtools::TestResult {
        let odb = odb()?;
        let tip = hex_to_id("62ed296d9986f50477e9f7b7e81cd0258939a43d");

        for sorting in [topo::Sorting::TopoOrder, topo::Sorting::DateOrder] {
            let expected = traverse_both([tip], [], &odb, sorting, Parents::All)?;
            let actual = traverse_both([tip, tip], [], &odb, sorting, Parents::All)?;
            assert_eq!(actual, expected, "duplicate tips must not affect the traversal");
        }
        Ok(())
    }

    #[test]
    fn one_end() -> gix_testtools::TestResult {
        let odb = odb()?;
        let tip = hex_to_id("62ed296d9986f50477e9f7b7e81cd0258939a43d");
        let end = hex_to_id("f1cce1b5c7efcdfa106e95caa6c45a2cae48a481");

        let expected = [
            "62ed296d9986f50477e9f7b7e81cd0258939a43d",
            "722bf6b8c3d9e3a11fa5100a02ed9b140e1d209c",
            "3be0c4c793c634c8fd95054345d4935d10a0879a",
            "2083b02a78e88b747e305b6ed3d5a861cf9fb73f",
            "302a5d0530ec688c241f32c2f2b61b964dd17bee",
            "d09384f312b03e4a1413160739805ff25e8fe99d",
            "22fbc169eeca3c9678fc7028aa80fad5ef49019f",
            "eeab3243aad67bc838fc4425f759453bf0b47785",
            "693c775700cf90bd158ee6e7f14dd1b7bd83a4ce",
            "33eb18340e4eaae3e3dcf80222b02f161cd3f966",
            "1a27cb1a26c9faed9f0d1975326fe51123ab01ed",
        ]
        .map(hex_to_id);

        let result = traverse_both([tip], [end], &odb, topo::Sorting::TopoOrder, Parents::All)?;
        assert_eq!(result, expected);
        Ok(())
    }

    #[test]
    fn empty_range() -> gix_testtools::TestResult {
        let odb = odb()?;
        let tip = hex_to_id("f1cce1b5c7efcdfa106e95caa6c45a2cae48a481");
        let end = hex_to_id("eeab3243aad67bc838fc4425f759453bf0b47785");

        let result = traverse_both([tip], [end], &odb, topo::Sorting::TopoOrder, Parents::All)?;
        assert!(result.is_empty());
        Ok(())
    }

    #[test]
    fn two_tips_two_ends() -> gix_testtools::TestResult {
        let odb = odb()?;
        let tips = [
            hex_to_id("d09384f312b03e4a1413160739805ff25e8fe99d"),
            hex_to_id("3be0c4c793c634c8fd95054345d4935d10a0879a"),
        ];
        let ends = [
            hex_to_id("1a27cb1a26c9faed9f0d1975326fe51123ab01ed"),
            hex_to_id("22fbc169eeca3c9678fc7028aa80fad5ef49019f"),
        ];

        let expected = [
            "3be0c4c793c634c8fd95054345d4935d10a0879a",
            "2083b02a78e88b747e305b6ed3d5a861cf9fb73f",
            "302a5d0530ec688c241f32c2f2b61b964dd17bee",
            "d09384f312b03e4a1413160739805ff25e8fe99d",
            "eeab3243aad67bc838fc4425f759453bf0b47785",
            "693c775700cf90bd158ee6e7f14dd1b7bd83a4ce",
            "33eb18340e4eaae3e3dcf80222b02f161cd3f966",
        ]
        .map(hex_to_id);

        let result = traverse_both(tips, ends, &odb, topo::Sorting::TopoOrder, Parents::All)?;
        assert_eq!(result, expected);
        Ok(())
    }

    #[test]
    fn with_dummy_predicate() -> gix_testtools::TestResult {
        let odb = odb()?;
        let tip = hex_to_id("62ed296d9986f50477e9f7b7e81cd0258939a43d");
        let filter_out = hex_to_id("eeab3243aad67bc838fc4425f759453bf0b47785");

        let expected = [
            "62ed296d9986f50477e9f7b7e81cd0258939a43d",
            "722bf6b8c3d9e3a11fa5100a02ed9b140e1d209c",
            "3be0c4c793c634c8fd95054345d4935d10a0879a",
            "2083b02a78e88b747e305b6ed3d5a861cf9fb73f",
            "302a5d0530ec688c241f32c2f2b61b964dd17bee",
            "d09384f312b03e4a1413160739805ff25e8fe99d",
            "22fbc169eeca3c9678fc7028aa80fad5ef49019f",
            "693c775700cf90bd158ee6e7f14dd1b7bd83a4ce",
            "33eb18340e4eaae3e3dcf80222b02f161cd3f966",
            "1a27cb1a26c9faed9f0d1975326fe51123ab01ed",
            "f1cce1b5c7efcdfa106e95caa6c45a2cae48a481",
            "945d8a360915631ad545e0cf04630d86d3d4eaa1",
            "a863c02247a6c5ba32dff5224459f52aa7f77f7b",
            "2f291881edfb0597493a52d26ea09dd7340ce507",
            "9c46b8765703273feb10a2ebd810e70b8e2ca44a",
            "fb3e21cf45b04b617011d2b30973f3e5ce60d0cd",
        ]
        .map(hex_to_id);

        let result = traverse_with_predicate([tip], [], &odb, topo::Sorting::TopoOrder, Parents::All, move |oid| {
            oid != filter_out
        })?;
        assert_eq!(result, expected);
        Ok(())
    }

    #[test]
    fn end_along_first_parent() -> gix_testtools::TestResult {
        let odb = odb()?;
        let tip = hex_to_id("d09384f312b03e4a1413160739805ff25e8fe99d");
        let end = hex_to_id("33eb18340e4eaae3e3dcf80222b02f161cd3f966");

        let expected = [
            "d09384f312b03e4a1413160739805ff25e8fe99d",
            "22fbc169eeca3c9678fc7028aa80fad5ef49019f",
            "eeab3243aad67bc838fc4425f759453bf0b47785",
            "693c775700cf90bd158ee6e7f14dd1b7bd83a4ce",
        ]
        .map(hex_to_id);

        let result = traverse_both([tip], [end], &odb, topo::Sorting::TopoOrder, Parents::All)?;
        assert_eq!(result, expected);
        Ok(())
    }
}

mod first_parent {
    use super::*;

    #[test]
    fn basic() -> gix_testtools::TestResult {
        let odb = odb()?;
        let tip = hex_to_id("62ed296d9986f50477e9f7b7e81cd0258939a43d");

        let expected = [
            "62ed296d9986f50477e9f7b7e81cd0258939a43d",
            "722bf6b8c3d9e3a11fa5100a02ed9b140e1d209c",
            "d09384f312b03e4a1413160739805ff25e8fe99d",
            "eeab3243aad67bc838fc4425f759453bf0b47785",
            "693c775700cf90bd158ee6e7f14dd1b7bd83a4ce",
            "33eb18340e4eaae3e3dcf80222b02f161cd3f966",
            "1a27cb1a26c9faed9f0d1975326fe51123ab01ed",
            "f1cce1b5c7efcdfa106e95caa6c45a2cae48a481",
            "945d8a360915631ad545e0cf04630d86d3d4eaa1",
            "a863c02247a6c5ba32dff5224459f52aa7f77f7b",
            "2f291881edfb0597493a52d26ea09dd7340ce507",
            "9c46b8765703273feb10a2ebd810e70b8e2ca44a",
            "fb3e21cf45b04b617011d2b30973f3e5ce60d0cd",
        ]
        .map(hex_to_id);

        let result = traverse_both([tip], [], &odb, topo::Sorting::TopoOrder, Parents::First)?;
        assert_eq!(result, expected);

        // Verify against baseline
        let baseline = read_baseline(&fixture_dir()?, "first-parent")?;
        let expected_strs: Vec<_> = expected.iter().map(std::string::ToString::to_string).collect();
        assert_eq!(expected_strs, baseline, "Baseline must match the expectation");

        Ok(())
    }

    #[test]
    fn with_end() -> gix_testtools::TestResult {
        let odb = odb()?;
        let tip = hex_to_id("62ed296d9986f50477e9f7b7e81cd0258939a43d");
        let end = hex_to_id("f1cce1b5c7efcdfa106e95caa6c45a2cae48a481");

        let expected = [
            "62ed296d9986f50477e9f7b7e81cd0258939a43d",
            "722bf6b8c3d9e3a11fa5100a02ed9b140e1d209c",
            "d09384f312b03e4a1413160739805ff25e8fe99d",
            "eeab3243aad67bc838fc4425f759453bf0b47785",
            "693c775700cf90bd158ee6e7f14dd1b7bd83a4ce",
            "33eb18340e4eaae3e3dcf80222b02f161cd3f966",
            "1a27cb1a26c9faed9f0d1975326fe51123ab01ed",
        ]
        .map(hex_to_id);

        let result = traverse_both([tip], [end], &odb, topo::Sorting::TopoOrder, Parents::First)?;
        assert_eq!(result, expected);
        Ok(())
    }

    #[test]
    fn end_is_second_parent() -> gix_testtools::TestResult {
        let odb = odb()?;
        let tip = hex_to_id("62ed296d9986f50477e9f7b7e81cd0258939a43d");
        let end = hex_to_id("3be0c4c793c634c8fd95054345d4935d10a0879a");

        let expected = [
            "62ed296d9986f50477e9f7b7e81cd0258939a43d",
            "722bf6b8c3d9e3a11fa5100a02ed9b140e1d209c",
            "d09384f312b03e4a1413160739805ff25e8fe99d",
            "eeab3243aad67bc838fc4425f759453bf0b47785",
            "693c775700cf90bd158ee6e7f14dd1b7bd83a4ce",
            "33eb18340e4eaae3e3dcf80222b02f161cd3f966",
            "1a27cb1a26c9faed9f0d1975326fe51123ab01ed",
        ]
        .map(hex_to_id);

        let result = traverse_both([tip], [end], &odb, topo::Sorting::TopoOrder, Parents::First)?;
        assert_eq!(result, expected);
        Ok(())
    }
}

mod date_order {
    use super::*;

    #[test]
    fn with_ends() -> gix_testtools::TestResult {
        let odb = odb()?;
        // Same tip and end as basic::one_end() but the order should be different.
        let tip = hex_to_id("62ed296d9986f50477e9f7b7e81cd0258939a43d");
        let end = hex_to_id("f1cce1b5c7efcdfa106e95caa6c45a2cae48a481");

        let expected = [
            "62ed296d9986f50477e9f7b7e81cd0258939a43d",
            "722bf6b8c3d9e3a11fa5100a02ed9b140e1d209c",
            "3be0c4c793c634c8fd95054345d4935d10a0879a",
            "2083b02a78e88b747e305b6ed3d5a861cf9fb73f",
            "302a5d0530ec688c241f32c2f2b61b964dd17bee",
            "d09384f312b03e4a1413160739805ff25e8fe99d",
            "eeab3243aad67bc838fc4425f759453bf0b47785",
            "22fbc169eeca3c9678fc7028aa80fad5ef49019f",
            "693c775700cf90bd158ee6e7f14dd1b7bd83a4ce",
            "33eb18340e4eaae3e3dcf80222b02f161cd3f966",
            "1a27cb1a26c9faed9f0d1975326fe51123ab01ed",
        ]
        .map(hex_to_id);

        let result = traverse_both([tip], [end], &odb, topo::Sorting::DateOrder, Parents::All)?;
        assert_eq!(result, expected);

        // Verify against baseline
        let baseline = read_baseline(&fixture_dir()?, "date-order")?;
        let expected_strs: Vec<_> = expected.iter().map(std::string::ToString::to_string).collect();
        assert_eq!(expected_strs, baseline, "Baseline must match the expectation");

        Ok(())
    }
}

mod admitted {
    use super::*;
    use std::{
        collections::BTreeMap,
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
    };

    #[derive(Default)]
    struct Admission {
        live: AtomicUsize,
        reservations: AtomicUsize,
        releases: AtomicUsize,
        cancelled: AtomicBool,
        denied: AtomicBool,
    }

    impl topo::Budget for Admission {
        fn check(&self) -> gix_error::Result {
            if self.cancelled.load(Ordering::SeqCst) {
                return Err(gix_error::cancelled("synthetic traversal cancelled").into());
            }
            Ok(())
        }

        fn reserve(&self, bytes: usize) -> gix_error::Result {
            if self.denied.load(Ordering::SeqCst) {
                return Err(gix_error::allocation_limit("synthetic admission denied").into());
            }
            assert_eq!(self.live.swap(bytes, Ordering::SeqCst), 0, "one reservation per traversal");
            self.reservations.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn release(&self, bytes: usize) {
            assert_eq!(self.live.swap(0, Ordering::SeqCst), bytes, "release the complete live reservation");
            self.releases.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[derive(Default)]
    struct Source {
        commits: BTreeMap<ObjectId, Vec<u8>>,
        reads: AtomicUsize,
    }

    impl Source {
        fn commit(&mut self, parents: &[ObjectId], time: i64) -> gix_error::Result<ObjectId> {
            let mut bytes = format!("tree {}\n", ObjectId::null(gix_hash::Kind::Sha1));
            for parent in parents {
                bytes.push_str(&format!("parent {parent}\n"));
            }
            bytes.push_str(&format!("author A <a@example.invalid> {time} +0000\ncommitter A <a@example.invalid> {time} +0000\n\nsynthetic\n"));
            let id = gix_object::compute_hash(gix_hash::Kind::Sha1, gix_object::Kind::Commit, bytes.as_bytes())?;
            self.commits.insert(id, bytes.into_bytes());
            Ok(id)
        }
    }

    impl gix_object::Find for Source {
        fn try_find<'a>(&self, id: &oid, out: &'a mut Vec<u8>) -> gix_error::Result<Option<gix_object::Data<'a>>> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            let Some(bytes) = self.commits.get(id) else { return Ok(None); };
            assert!(out.capacity() >= bytes.len(), "Find must receive its pre-admitted backing");
            out.clear();
            out.extend_from_slice(bytes);
            Ok(Some(gix_object::Data::new(out, gix_object::Kind::Commit, id.kind())))
        }
    }

    fn graph() -> gix_error::Result<(Source, [ObjectId; 4])> {
        let mut source = Source::default();
        let root = source.commit(&[], 900)?;
        let left = source.commit(&[root], 1)?;
        let right = source.commit(&[root], 1000)?;
        let tip = source.commit(&[left, right], 2)?;
        Ok((source, [root, left, right, tip]))
    }

    fn limits() -> topo::Limits {
        topo::Limits { commits: 4, edges: 4, buffer_bytes: 4096 }
    }

    #[test]
    fn returned_parent_list_retains_admission_after_the_iterator_is_destroyed() -> gix_testtools::TestResult {
        let (source, [_, left, right, tip]) = graph()?;
        let budget = Arc::new(Admission::default());
        let mut walk = topo::Builder::new(&source).with_tips([tip]).sorting(topo::Sorting::TopoOrder)
            .with_admission(budget.clone(), limits())?.build_admitted()?;
        let info = walk.next().expect("the tip exists")?;
        assert_eq!(info.id, tip);
        assert_eq!(info.parent_ids.as_slice(), [left, right]);
        drop(walk);
        assert_ne!(budget.live.load(Ordering::SeqCst), 0, "the returned heap-backed parents are still live");
        assert_eq!(budget.releases.load(Ordering::SeqCst), 0);
        drop(info);
        assert_eq!(budget.live.load(Ordering::SeqCst), 0);
        assert_eq!(budget.reservations.load(Ordering::SeqCst), 1);
        assert_eq!(budget.releases.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[test]
    fn controlled_order_keeps_merge_ancestry_despite_reversed_commit_clocks() -> gix_testtools::TestResult {
        let (source, [root, left, right, tip]) = graph()?;
        let budget = Arc::new(Admission::default());
        let walk = topo::Builder::new(&source).with_tips([tip]).sorting(topo::Sorting::TopoOrder)
            .with_admission(budget.clone(), limits())?.build_admitted()?;
        let actual = walk.map(|info| info.map(|info| info.id)).collect::<gix_error::Result<Vec<_>>>()?;
        assert_eq!(actual, [tip, right, left, root]);
        assert_eq!(budget.live.load(Ordering::SeqCst), 0);
        Ok(())
    }

    #[test]
    fn admission_denial_and_plain_build_do_not_read_or_detach_parent_storage() -> gix_testtools::TestResult {
        let (source, [_, _, _, tip]) = graph()?;
        let budget = Arc::new(Admission::default());
        budget.denied.store(true, Ordering::SeqCst);
        let error = topo::Builder::new(&source).with_tips([tip]).with_admission(budget.clone(), limits())
            .err().expect("denied backing cannot construct a controlled builder");
        assert!(error.to_string().contains("admission denied"), "{error}");
        assert_eq!(source.reads.load(Ordering::SeqCst), 0);
        assert_eq!(budget.reservations.load(Ordering::SeqCst), 0);

        budget.denied.store(false, Ordering::SeqCst);
        let error = topo::Builder::new(&source).with_tips([tip]).with_admission(budget.clone(), limits())?
            .build().err().expect("controlled backing cannot escape through ordinary Info values");
        assert!(error.to_string().contains("build_admitted"), "{error}");
        assert_eq!(source.reads.load(Ordering::SeqCst), 0);
        assert_eq!(budget.live.load(Ordering::SeqCst), 0);
        assert_eq!(budget.releases.load(Ordering::SeqCst), 1);
        Ok(())
    }

    #[test]
    fn understated_commit_or_parent_inventory_fails_without_leaking_admission() -> gix_testtools::TestResult {
        let (source, [_, _, _, tip]) = graph()?;
        for limits in [topo::Limits { commits: 3, ..limits() }, topo::Limits { edges: 3, ..limits() }] {
            let budget = Arc::new(Admission::default());
            let error = topo::Builder::new(&source).with_tips([tip]).with_admission(budget.clone(), limits)?
                .build_admitted().err().expect("the physical inventory exceeds the declared bound");
            assert!(error.to_string().contains("bound exceeded"), "{error}");
            assert_eq!(budget.live.load(Ordering::SeqCst), 0);
            assert_eq!(budget.releases.load(Ordering::SeqCst), 1);
        }
        Ok(())
    }

    #[test]
    fn cancellation_after_build_stops_before_another_physical_read() -> gix_testtools::TestResult {
        let (source, [_, _, _, tip]) = graph()?;
        let budget = Arc::new(Admission::default());
        let mut walk = topo::Builder::new(&source).with_tips([tip])
            .with_admission(budget.clone(), limits())?.build_admitted()?;
        let before = source.reads.load(Ordering::SeqCst);
        budget.cancelled.store(true, Ordering::SeqCst);
        let error = walk.next().expect("cancellation is reported").err().expect("cancelled work yields no commit");
        assert!(error.is_cancelled(), "{error}");
        assert_eq!(source.reads.load(Ordering::SeqCst), before);
        drop(walk);
        assert_eq!(budget.live.load(Ordering::SeqCst), 0);
        Ok(())
    }

    #[test]
    fn repeated_ends_exclude_the_tip_without_duplicating_physical_queue_entries() -> gix_testtools::TestResult {
        let (source, [_, _, _, tip]) = graph()?;
        let budget = Arc::new(Admission::default());
        let mut walk = topo::Builder::new(&source).with_tips([tip]).with_ends([tip; 1023])
            .with_admission(budget.clone(), limits())?.build_admitted()?;
        assert!(walk.next().is_none(), "an explicit end excludes the tip itself");
        drop(walk);
        assert_eq!(budget.live.load(Ordering::SeqCst), 0);
        Ok(())
    }
}
