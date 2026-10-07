use jig_core_views::{Chunk, End, Event, Request};
use jig_views_world::Referee;
use skein_lib::{Time, Token};

fn admitted(referee: &mut Referee) {
    referee.saw(&[
        Request::Watching { watcher: Token::new(1) },
        Request::Deliver {
            watcher: Token::new(1),
            missed: 0,
            chunks: Box::new([Chunk::Snapshot { at: Time::ZERO, content: Box::new([]) }]),
        },
    ]);
}

#[test]
fn a_watch_with_a_snapshot_and_one_delivery_at_a_time_passes() {
    let mut referee = Referee::default();
    admitted(&mut referee);
    referee.before(&Event::Delivered { watcher: Token::new(1), done: true });
    referee.saw(&[Request::Deliver {
        watcher: Token::new(1),
        missed: 1,
        chunks: Box::new([Chunk::Phase { task: Token::new(7), phase: 2, at: Time::ZERO }]),
    }]);
    referee.before(&Event::Delivered { watcher: Token::new(1), done: true });
    referee.saw(&[Request::Ended { watcher: Token::new(1), end: End::Unwatched }]);
    assert_eq!((referee.deliveries, referee.missed, referee.ended, referee.live()), (2, 1, 1, 0));
}

#[test]
fn a_second_delivery_in_flight_fails_the_referee() {
    let result = std::panic::catch_unwind(|| {
        let mut referee = Referee::default();
        admitted(&mut referee);
        referee.saw(&[Request::Deliver {
            watcher: Token::new(1),
            missed: 0,
            chunks: Box::new([Chunk::Phase { task: Token::new(7), phase: 2, at: Time::ZERO }]),
        }]);
    });
    assert!(result.is_err());
}

#[test]
fn ending_before_the_delivery_terminal_fails_the_referee() {
    let result = std::panic::catch_unwind(|| {
        let mut referee = Referee::default();
        admitted(&mut referee);
        referee.saw(&[Request::Ended { watcher: Token::new(1), end: End::Finished }]);
    });
    assert!(result.is_err());
}

#[test]
fn an_initial_delivery_without_a_snapshot_fails_the_referee() {
    let result = std::panic::catch_unwind(|| {
        let mut referee = Referee::default();
        referee.saw(&[
            Request::Watching { watcher: Token::new(1) },
            Request::Deliver {
                watcher: Token::new(1),
                missed: 0,
                chunks: Box::new([Chunk::Phase { task: Token::new(7), phase: 2, at: Time::ZERO }]),
            },
        ]);
    });
    assert!(result.is_err());
}
