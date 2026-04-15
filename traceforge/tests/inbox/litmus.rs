use traceforge::{self, thread, Config, ConsType};

#[derive(Clone, Debug, PartialEq)]
struct Msg {}

macro_rules! equiv_test {
    ($I_case:ident, $R_case:ident) => {
        let I_stats = traceforge::verify(Config::builder().build(), $I_case);
        let R_stats = traceforge::verify(Config::builder().build(), $R_case);
        assert_eq!(I_stats.execs, R_stats.execs);
        assert_eq!(I_stats.block, R_stats.block);
    };
}

macro_rules! not_equiv_test {
    ($I_case:ident, $R_case:ident) => {
        let I_stats = traceforge::verify(Config::builder().build(), $I_case);
        let R_stats = traceforge::verify(Config::builder().build(), $R_case);
        assert!(I_stats.execs != R_stats.execs || I_stats.block != R_stats.block);
    };
}

#[test]
fn I_eq_Rnb() {
    let I_case = move || {
        let _ = traceforge::inbox();
    };
    let R_case = move || {
        let _: Option<Msg> = traceforge::recv_msg();
    };
    equiv_test!(I_case, R_case);
}

#[test]
fn I11_eq_Rb() {
    let I_case = move || {
        let _ = traceforge::inbox_with_bounds(1, Some(1));
    };
    let R_case = move || {
        let _: Msg = traceforge::recv_msg_block();
    };
    equiv_test!(I_case, R_case);
}

#[test]
fn I11_neq_Rnb() {
    let I_case = move || {
        let _ = traceforge::inbox_with_bounds(1, Some(1));
    };
    let R_case = move || {
        let _: Option<Msg> = traceforge::recv_msg();
    };
    not_equiv_test!(I_case, R_case);
}

#[test]
fn I_neq_Rb() {
    let I_case = move || {
        let _ = traceforge::inbox();
    };
    let R_case = move || {
        let _: Msg = traceforge::recv_msg_block();
    };
    not_equiv_test!(I_case, R_case);
}

#[test]
fn SI_eq_SRnb() {
    let I_case = move || {
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
        let _ = traceforge::inbox();
    };
    let R_case = move || {
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
        let _: Option<Msg> = traceforge::recv_msg();
    };
    equiv_test!(I_case, R_case);
}

#[test]
fn IS_eq_RnbS() {
    let I_case = move || {
        let _ = traceforge::inbox();
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
    };
    let R_case = move || {
        let _: Option<Msg> = traceforge::recv_msg();
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
    };
    equiv_test!(I_case, R_case);
}

#[test]
fn SI_neq_SRb() {
    let I_case = move || {
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
        let _ = traceforge::inbox();
    };
    let R_case = move || {
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
        let _: Msg = traceforge::recv_msg_block();
    };
    not_equiv_test!(I_case, R_case);
}

#[test]
fn I11S_neq_RnbS() {
    let I_case = move || {
        let _ = traceforge::inbox_with_bounds(1, Some(1));
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
    };
    let R_case = move || {
        let _: Option<Msg> = traceforge::recv_msg();
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
    };
    not_equiv_test!(I_case, R_case);
}

#[test]
fn test_I2S() {
    let stats = traceforge::verify(Config::builder().build(), move || {
        let _ = traceforge::inbox();
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
    });
    assert_eq!(stats.execs, 1);
    assert_eq!(stats.block, 0);
}

#[test]
fn test_SIS() {
    let stats = traceforge::verify(Config::builder().build(), move || {
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
        let _ = traceforge::inbox();
        traceforge::send_msg(traceforge::thread::main_thread_id(), Msg {});
    });
    assert_eq!(stats.execs, 2);
    assert_eq!(stats.block, 0);
}

#[test]
fn test_2SI() {
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
fn test_nSIjS() {
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
fn test_2SRnbI() {
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
fn test_nSRnbI() {
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
fn test_nSIRnb() {
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
fn test_RnbInS() {
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
fn test_IRnbnS() {
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
fn test_2SRnbI2SIRn() {
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
fn test_2SRnbI1inf() {
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
fn test_2SRnbI2inf() {
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
fn test_2SRnbI1x1() {
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
fn test_2SRnbI1x2() {
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
fn test_2SRnbI2x2() {
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
fn test_S_I1_SS() {
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

            inbox_thread.join();
            s2_thread.join();
            s3_thread.join();
        },
    );
    assert_eq!(stats.execs, 7);
    assert_eq!(stats.block, 0);
}

#[test]
fn test_S_I11_S() {
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
fn test_S_I22_SS() {
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
fn test_S_I1x2_SS() {
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
fn test_I0_SS() {
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
fn test_SA_SB_Ia1_SA() {
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
fn test_S_2I11_S() {
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
