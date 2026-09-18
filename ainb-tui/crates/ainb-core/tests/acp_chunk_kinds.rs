// ABOUTME: Every ACP transcript kind the daemon's reducer writes has a kind of
// its own on the frame. The match is exhaustive over `ainb_acp`'s enum, so a
// new `acp.*` kind fails to compile here instead of drawing as a lifecycle row.

use ainb_acp::reducer::ChunkKind as Written;
use ainb_app::fleet::transcript::ChunkKind as Framed;

#[test]
fn every_kind_the_daemon_writes_has_its_own_kind_on_the_frame() {
    let all = [
        Written::Message,
        Written::UserMessage,
        Written::Thought,
        Written::ToolCall,
        Written::Plan,
        Written::Permission,
        Written::Usage,
    ];
    for written in all {
        let framed = match written {
            Written::Message => Framed::Message,
            Written::UserMessage => Framed::UserMessage,
            Written::Thought => Framed::Thought,
            Written::ToolCall => Framed::ToolCall,
            Written::Plan => Framed::Plan,
            Written::Permission => Framed::Permission,
            Written::Usage => Framed::Usage,
        };
        assert_eq!(Framed::of(written.event_type()), framed, "{written:?}");
    }
}
