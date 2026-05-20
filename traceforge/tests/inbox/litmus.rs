use traceforge::{self, thread, Config, ConsType};

#[derive(Clone, Debug, PartialEq)]
struct Msg {}

macro_rules! equiv_test {
    ($i_case:ident, $r_case:ident) => {
        let i_stats = traceforge::verify(Config::builder().build(), $i_case);
        let r_stats = traceforge::verify(Config::builder().build(), $r_case);
        assert_eq!(i_stats.execs, r_stats.execs);
        assert_eq!(i_stats.block, r_stats.block);
    };
}

macro_rules! not_equiv_test {
    ($i_case:ident, $r_case:ident) => {
        let i_stats = traceforge::verify(Config::builder().build(), $i_case);
        let r_stats = traceforge::verify(Config::builder().build(), $r_case);
        assert!(i_stats.execs != r_stats.execs || i_stats.block != r_stats.block);
    };
}

#[test]
fn i_eq_rnb() {
    let i_case = move || {
        let _ = traceforge::inbox();
    };
    let r_case = move || {
        let _: Option<Msg> = traceforge::recv_msg();
    };
    equiv_test!(i_case, r_case);
}

#[test]
fn i11_eq_rb() {
    let i_case = move || {
        let _ = traceforge::inbox_with_bounds(1, Some(1));
    };
    let r_case = move || {
        let _: Msg = traceforge::recv_msg_block();
    };
    equiv_test!(i_case, r_case);
}

#[test]
fn i11_neq_rnb() {
    let i_case = move || {
        let _ = traceforge::inbox_with_bounds(1, Some(1));
    };
    let r_case = move || {
        let _: Option<Msg> = traceforge::recv_msg();
    };
    not_equiv_test!(i_case, r_case);
}

#[test]
fn i_neq_rb() {
    let i_case = move || {
        let _ = traceforge::inbox();
    };
    let r_case = move || {
        let _: Msg = traceforge::recv_msg_block();
    };
    not_equiv_test!(i_case, r_case);
}

#[test]
fn si_eq_srnb() {
    let i_case = move || {
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
        let _ = traceforge::inbox();
    };
    let r_case = move || {
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
        let _: Option<Msg> = traceforge::recv_msg();
    };
    equiv_test!(i_case, r_case);
}

#[test]
fn is_eq_rnb_s() {
    let i_case = move || {
        let _ = traceforge::inbox();
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
    };
    let r_case = move || {
        let _: Option<Msg> = traceforge::recv_msg();
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
    };
    equiv_test!(i_case, r_case);
}

#[test]
fn si_neq_srb() {
    let i_case = move || {
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
        let _ = traceforge::inbox();
    };
    let r_case = move || {
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
        let _: Msg = traceforge::recv_msg_block();
    };
    not_equiv_test!(i_case, r_case);
}

#[test]
fn i11_s_neq_rnb_s() {
    let i_case = move || {
        let _ = traceforge::inbox_with_bounds(1, Some(1));
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
    };
    let r_case = move || {
        let _: Option<Msg> = traceforge::recv_msg();
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
    };
    not_equiv_test!(i_case, r_case);
}

#[test]
fn test_i2_s() {
    let stats = traceforge::verify(Config::builder().build(), move || {
        let _ = traceforge::inbox();
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
    });
    assert_eq!(stats.execs, 1);
    assert_eq!(stats.block, 0);
}

#[test]
fn test_sis() {
    let stats = traceforge::verify(Config::builder().build(), move || {
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
        let _ = traceforge::inbox();
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
    });
    assert_eq!(stats.execs, 2);
    assert_eq!(stats.block, 0);
}

#[test]
fn test_2_si() {
    let stats = traceforge::verify(
        Config::builder().with_cons_type(ConsType::Bag).build(),
        move || {
            traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
            traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
            let _ = traceforge::inbox();
        },
    );
    assert_eq!(stats.execs, 4);
    assert_eq!(stats.block, 0);
}

#[test]
fn test_n_sij_s() {
    for n in 0..4u32 {
        for j in 0..4u32 {
            let stats = traceforge::verify(
                Config::builder().with_cons_type(ConsType::Bag).build(),
                move || {
                    let id = traceforge::thread::main_thread_id();
                    for _ in 0..n {
                        traceforge::send_msg(id, Msg {});
                    }
                    let _ = traceforge::inbox();
                    for _ in 0..j {
                        traceforge::send_msg(id, Msg {});
                    }
                },
            );
            assert_eq!(stats.execs, (1 + 1u32).pow(n) as usize);
            assert_eq!(stats.block, 0);
        }
    }
}

#[test]
fn test_2_srnb_i() {
    let stats = traceforge::verify(
        Config::builder().with_cons_type(ConsType::Bag).build(),
        move || {
            let id = traceforge::thread::main_thread_id();
            for _ in 0..2 {
                traceforge::send_msg(id, Msg {});
            }
            let _: Option<Msg> = traceforge::recv_msg();
            let _ = traceforge::inbox();
        },
    );
    assert_eq!(stats.execs, 8);
    assert_eq!(stats.block, 0);
}

#[test]
fn test_n_srnb_i() {
    for n in 1..4u32 {
        let stats = traceforge::verify(
            Config::builder().with_cons_type(ConsType::Bag).build(),
            move || {
                let id = traceforge::thread::main_thread_id();
                for _ in 0..n {
                    traceforge::send_msg(id, Msg {});
                }
                let _: Option<Msg> = traceforge::recv_msg();
                let _ = traceforge::inbox();
            },
        );
        assert_eq!(stats.execs, (2u32.pow(n) + n * 2u32.pow(n - 1)) as usize);
        assert_eq!(stats.block, 0);
    }
}

#[test]
fn test_n_sirnb() {
    for n in 1..4u32 {
        let stats = traceforge::verify(
            Config::builder().with_cons_type(ConsType::Bag).build(),
            move || {
                let id = traceforge::thread::main_thread_id();
                for _ in 0..n {
                    traceforge::send_msg(id, Msg {});
                }
                let _ = traceforge::inbox();
                let _: Option<Msg> = traceforge::recv_msg();
            },
        );
        assert_eq!(stats.execs, (2u32.pow(n) + n * 2u32.pow(n - 1)) as usize);
        assert_eq!(stats.block, 0);
    }
}

#[test]
fn test_rnb_in_s() {
    for n in 1..4u32 {
        let stats = traceforge::verify(
            Config::builder().with_cons_type(ConsType::Bag).build(),
            move || {
                let id = traceforge::thread::main_thread_id();
                let _: Option<Msg> = traceforge::recv_msg();
                let _ = traceforge::inbox();
                for _ in 0..n {
                    traceforge::send_msg(id, Msg {});
                }
            },
        );
        assert_eq!(stats.execs, 1);
        assert_eq!(stats.block, 0);
    }
}

#[test]
fn test_irnbn_s() {
    for n in 1..4u32 {
        let stats = traceforge::verify(
            Config::builder().with_cons_type(ConsType::Bag).build(),
            move || {
                let id = traceforge::thread::main_thread_id();
                let _ = traceforge::inbox();
                let _: Option<Msg> = traceforge::recv_msg();
                for _ in 0..n {
                    traceforge::send_msg(id, Msg {});
                }
            },
        );
        assert_eq!(stats.execs, 1);
        assert_eq!(stats.block, 0);
    }
}

#[test]
fn test_2_srnb_i2_sirn() {
    let stats = traceforge::verify(
        Config::builder().with_cons_type(ConsType::Bag).build(),
        move || {
            let id = traceforge::thread::main_thread_id();
            for _ in 0..2 {
                traceforge::send_msg(id, Msg {});
            }
            let _: Option<Msg> = traceforge::recv_msg();
            let _ = traceforge::inbox();
            for _ in 0..2 {
                traceforge::send_msg(id, Msg {});
            }
            let _ = traceforge::inbox();
            let _: Option<Msg> = traceforge::recv_msg();
        },
    );
    assert_eq!(
        stats.execs,
        (3 * (2i32.pow(2) + 2 * 2i32.pow(1))
            + 4 * (2i32.pow(3) + 3 * 2i32.pow(2))
            + 1 * (2i32.pow(4) + 4 * 2i32.pow(3))) as usize
    );
    assert_eq!(stats.block, 0);
}

#[test]
fn test_2_srnb_i1inf() {
    let stats = traceforge::verify(
        Config::builder().with_cons_type(ConsType::Bag).build(),
        move || {
            let id = traceforge::thread::main_thread_id();
            for _ in 0..2 {
                traceforge::send_msg(id, Msg {});
            }
            let _: Option<Msg> = traceforge::recv_msg();
            let _ = traceforge::inbox_with_bounds(1, None);
        },
    );
    assert_eq!(stats.execs, 5);
    assert_eq!(stats.block, 0);
}

#[test]
fn test_2_srnb_i2inf() {
    let stats = traceforge::verify(
        Config::builder().with_cons_type(ConsType::Bag).build(),
        move || {
            let id = traceforge::thread::main_thread_id();
            for _ in 0..2 {
                traceforge::send_msg(id, Msg {});
            }
            let _: Option<Msg> = traceforge::recv_msg();
            let _ = traceforge::inbox_with_bounds(2, None);
        },
    );
    assert_eq!(stats.execs, 1);
    assert_eq!(stats.block, 2);
}

#[test]
fn test_2_srnb_i1x1() {
    let stats = traceforge::verify(
        Config::builder().with_cons_type(ConsType::Bag).build(),
        move || {
            let id = traceforge::thread::main_thread_id();
            for _ in 0..2 {
                traceforge::send_msg(id, Msg {});
            }
            let _: Option<Msg> = traceforge::recv_msg();
            let _ = traceforge::inbox_with_bounds(1, Some(1));
        },
    );
    assert_eq!(stats.execs, 4);
    assert_eq!(stats.block, 0);
}

#[test]
fn test_2_srnb_i1x2() {
    let stats = traceforge::verify(
        Config::builder().with_cons_type(ConsType::Bag).build(),
        move || {
            let id = traceforge::thread::main_thread_id();
            for _ in 0..2 {
                traceforge::send_msg(id, Msg {});
            }
            let _: Option<Msg> = traceforge::recv_msg();
            let _ = traceforge::inbox_with_bounds(1, Some(2));
        },
    );
    assert_eq!(stats.execs, 5);
    assert_eq!(stats.block, 0);
}

#[test]
fn test_2_srnb_i2x2() {
    let stats = traceforge::verify(
        Config::builder().with_cons_type(ConsType::Bag).build(),
        move || {
            let id = traceforge::thread::main_thread_id();
            for _ in 0..2 {
                traceforge::send_msg(id, Msg {});
            }
            let _: Option<Msg> = traceforge::recv_msg();
            let _ = traceforge::inbox_with_bounds(2, Some(2));
        },
    );
    assert_eq!(stats.execs, 1);
    assert_eq!(stats.block, 2);
}

#[test]
fn test_s_i1_ss() {
    let stats = traceforge::verify(
        Config::builder().with_cons_type(ConsType::Bag).build(),
        move || {
            let inbox_thread = thread::spawn(move || {
                let _ = traceforge::inbox_with_bounds(1, None);
            });
            let inbox_tid = inbox_thread.thread().id();
            traceforge::send_msg(inbox_tid, Msg {});

            let s2_thread = thread::spawn(move || {
                traceforge::send_msg(inbox_tid, Msg {});
            });

            let s3_thread = thread::spawn(move || {
                traceforge::send_msg(inbox_tid, Msg {});
            });

            let _ = inbox_thread.join();
            let _ = s2_thread.join();
            let _ = s3_thread.join();
        },
    );
    assert_eq!(stats.execs, 7);
    assert_eq!(stats.block, 0);
}

#[test]
fn test_s_i11_s() {
    let stats = traceforge::verify(
        Config::builder().with_cons_type(ConsType::Bag).build(),
        move || {
            let inbox_thread = thread::spawn(move || {
                let _ = traceforge::inbox_with_bounds(1, Some(1));
            });
            let inbox_tid = inbox_thread.thread().id();
            traceforge::send_msg(inbox_tid, Msg {});

            let s2_thread = thread::spawn(move || {
                traceforge::send_msg(inbox_tid, Msg {});
            });

            let _ = inbox_thread.join();
            let _ = s2_thread.join();
        },
    );
    assert_eq!(stats.execs, 2);
    assert_eq!(stats.block, 0);
}

#[test]
fn test_s_i22_ss() {
    let stats = traceforge::verify(
        Config::builder().with_cons_type(ConsType::Bag).build(),
        move || {
            let inbox_thread = thread::spawn(move || {
                let _ = traceforge::inbox_with_bounds(2, Some(2));
            });
            let inbox_tid = inbox_thread.thread().id();
            traceforge::send_msg(inbox_tid, Msg {});

            let s2_thread = thread::spawn(move || {
                traceforge::send_msg(inbox_tid, Msg {});
            });
            let s3_thread = thread::spawn(move || {
                traceforge::send_msg(inbox_tid, Msg {});
            });

            let _ = inbox_thread.join();
            let _ = s2_thread.join();
            let _ = s3_thread.join();
        },
    );
    assert_eq!(stats.execs, 3);
    assert_eq!(stats.block, 0);
}

#[test]
fn test_s_i1x2_ss() {
    let stats = traceforge::verify(
        Config::builder().with_cons_type(ConsType::Bag).build(),
        move || {
            let inbox_thread = thread::spawn(move || {
                let _ = traceforge::inbox_with_bounds(1, Some(2));
            });
            let inbox_tid = inbox_thread.thread().id();
            traceforge::send_msg(inbox_tid, Msg {});

            let s2_thread = thread::spawn(move || {
                traceforge::send_msg(inbox_tid, Msg {});
            });
            let s3_thread = thread::spawn(move || {
                traceforge::send_msg(inbox_tid, Msg {});
            });

            let _ = inbox_thread.join();
            let _ = s2_thread.join();
            let _ = s3_thread.join();
        },
    );
    assert_eq!(stats.execs, 6);
    assert_eq!(stats.block, 0);
}

#[test]
fn test_i0_ss() {
    let stats = traceforge::verify(
        Config::builder().with_cons_type(ConsType::Bag).build(),
        move || {
            let inbox_thread = thread::spawn(move || {
                let _ = traceforge::inbox();
            });
            let inbox_tid = inbox_thread.thread().id();

            let s1_thread = thread::spawn(move || {
                traceforge::send_msg(inbox_tid, Msg {});
            });
            let s2_thread = thread::spawn(move || {
                traceforge::send_msg(inbox_tid, Msg {});
            });

            let _ = inbox_thread.join();
            let _ = s1_thread.join();
            let _ = s2_thread.join();
        },
    );
    assert_eq!(stats.execs, 4);
    assert_eq!(stats.block, 0);
}

#[test]
fn test_sa_sb_ia1_sa() {
    let stats = traceforge::verify(
        Config::builder().with_cons_type(ConsType::Bag).build(),
        move || {
            let inbox_thread = thread::spawn(move || {
                let _ = traceforge::inbox_with_tag_and_bounds(
                    |_tid, tag| tag == Some(1),
                    1,
                    Some(1),
                );
            });
            let inbox_tid = inbox_thread.thread().id();
            traceforge::send_tagged_msg(inbox_tid, 1, Msg {});

            let sb_thread = thread::spawn(move || {
                traceforge::send_tagged_msg(inbox_tid, 2, Msg {});
            });
            let sa_thread = thread::spawn(move || {
                traceforge::send_tagged_msg(inbox_tid, 1, Msg {});
            });

            let _ = inbox_thread.join();
            let _ = sb_thread.join();
            let _ = sa_thread.join();
        },
    );
    assert_eq!(stats.execs, 2);
    assert_eq!(stats.block, 0);
}

#[test]
fn test_s_2_i11_s() {
    let stats = traceforge::verify(
        Config::builder().with_cons_type(ConsType::Bag).build(),
        move || {
            let inbox_thread = thread::spawn(move || {
                let _ = traceforge::inbox_with_bounds(1, Some(1));
                let _ = traceforge::inbox_with_bounds(1, Some(1));
            });
            let inbox_tid = inbox_thread.thread().id();
            traceforge::send_msg(inbox_tid, Msg {});

            let s2_thread = thread::spawn(move || {
                traceforge::send_msg(inbox_tid, Msg {});
            });

            let _ = inbox_thread.join();
            let _ = s2_thread.join();
        },
    );
    assert_eq!(stats.execs, 2);
    assert_eq!(stats.block, 0);
}
