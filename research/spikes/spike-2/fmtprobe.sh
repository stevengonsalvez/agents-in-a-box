#!/bin/bash
SD="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
T() { tmux -L ainb-spike2 -f /dev/null "$@"; }
T kill-session -t fp 2>/dev/null; sleep 0.3
T new-session -d -s fp -x 120 -y 40 -n w0 "sh $SD/fixtures/f11-altlink.sh"
T set-option -g window-size manual; T resize-window -t fp -x 120 -y 40
sleep 2
echo "--- does capture-pane -e carry OSC 8? ---"
T capture-pane -p -e -t fp | sed -n '2p' | cat -v | head -2
echo "--- terminal state tmux exposes as formats ---"
T display -p -t fp 'alt=#{alternate_on} title=#{pane_title} cursor=#{cursor_flag} x=#{cursor_x} y=#{cursor_y} keypad_cursor=#{keypad_cursor_flag} keypad=#{keypad_flag} wrap=#{wrap_flag} origin=#{origin_flag} insert=#{insert_flag} scroll=#{scroll_region_upper}-#{scroll_region_lower} m_any=#{mouse_any_flag} m_button=#{mouse_button_flag} m_std=#{mouse_standard_flag} m_sgr=#{mouse_sgr_flag} m_utf8=#{mouse_utf8_flag}'
T kill-session -t fp 2>/dev/null; true
