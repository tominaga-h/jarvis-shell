#!/bin/zsh
# jarvish-owned warm completion daemon init script (Task 2b.3, #89).
#
# Derived from the *inner* init block of assets/zsh/capture.zsh (the block
# that capture.zsh pipes into its throwaway zpty'd inner zsh via
# `=( <<< '...' )`), which is itself vendored from
# https://github.com/Valodim/zsh-capture-completion (MIT, Vincent
# Breitmoser and contributors). The `compadd` override (the -O/-A/-D
# delegate check, -A __hits -D __dscr capture, prefix/suffix
# reconstruction, "value -- description" echo lines) below is reused
# verbatim from that inner block.
#
# Unlike capture.zsh, this script is NOT sourced once inside a throwaway
# zpty'd shell that exits after a single completion. It is sourced ONCE
# directly into a persistent `zsh -i` process (spawned and owned by
# src/cli/completer/zsh_daemon.rs) that jarvish keeps alive across many
# Tab presses, so:
#   - `comppostfuncs` intentionally omits the trailing `exit` that
#     capture.zsh's inner block has (see capture.zsh:45) — the shell must
#     keep running after each completion.
#   - `bindkey '^U' kill-whole-line` is bound EXPLICITLY (capture.zsh's
#     inner zsh never needs this — it only ever runs ONE completion before
#     exiting). The daemon protocol requires it: each request first sends
#     ^U to discard whatever was left on the line by the previous request,
#     then the new (already zsh-escaped) line, then ^I to trigger
#     completion. Without an explicit bind here this would silently rely
#     on zsh's compiled-in default binding for ^U, which is technically
#     already kill-whole-line on most builds, but pinning it explicitly
#     makes the protocol's core safety property (no state bleed between
#     requests) independent of upstream zsh defaults.
#
# # Protocol notes learned empirically while prototyping this script
# (recorded here per the task's "record what you learned" requirement;
# prototype driver + raw transcripts lived under the task scratchpad,
# not committed):
#
# 1. `_main_complete` (the real zsh completion-system entry point that
#    `complete-word` invokes) treats `compprefuncs` / `comppostfuncs` as
#    ONE-SHOT arrays: it reads them, runs every function in them, and then
#    clears the array (`funcs=("$compprefuncs[@]"); compprefuncs=()`, and
#    the mirrored `comppostfuncs=()` at the end of the function — confirm
#    with `autoload -Uz +X _main_complete` on any system zsh). capture.zsh
#    never notices this because it only ever completes once per process.
#    A persistent daemon DOES notice: sourcing this script once and then
#    pressing Tab twice would sentinel-frame only the FIRST completion —
#    the second (and every later) completion would emit candidates with
#    NO leading/trailing NUL marker at all, desynchronizing the reader.
#    Empirically verified (scratchpad prototype): with `compprefuncs` /
#    `comppostfuncs` set only once at init, request #1 produced
#    `\x00...\r\nalpha\r\nbeta\r\ngamma\r\n\x00...`, but request #2 (even
#    for an entirely different command) produced bare
#    `alpha\r\nbeta\r\ngamma\r\n` with no sentinel bytes at all.
#
# 2. Fix: never rely on a one-time `compprefuncs=(...)` assignment. Instead
#    bind Tab to a small wrapper ZLE widget (`jarvish-complete-word` below)
#    that RE-ARMS both arrays immediately before delegating to the real
#    `complete-word` widget on every single invocation. This makes every
#    completion — first or hundredth — sentinel-framed identically, which
#    is what makes the outer NUL-toggle reader in zsh_daemon.rs able to
#    reuse the exact same framing logic capture.zsh's one-shot output used.
#
# 3. `bindkey '^U' kill-whole-line` genuinely resets zsh's line editor
#    state (`$BUFFER`, and therefore `$words`/`$CURRENT` as seen by the
#    next completion) with no leftover bleed from the previous request —
#    empirically verified by driving a long multi-word line through
#    completion, then ^U + a short unrelated line, and confirming the
#    completion oracle's `$words`/`$CURRENT` reflect ONLY the new line.
#
# 4. Once `compinit` has run at init time, warm completions on a fixture
#    command measured consistently in the tens-of-milliseconds range
#    (~55-60ms end-to-end including the toggle-frame read), independent of
#    request order or of whether the previous request used a different
#    command/line — matching this task's measured motivation that the
#    bulk of the one-shot bridge's 700-1100ms cost is zpty/PTY setup and
#    process spawn, not the completion computation itself.

# no prompt! (keeps the daemon's PTY output free of prompt noise between
# completions — same rationale as capture.zsh)
PROMPT=

# load completion system once, at daemon startup.
autoload compinit
compinit -d ~/.zcompdump_capture

# never run a command (Enter/Linefeed must never execute the accumulated
# buffer — the daemon only ever uses the buffer as a completion scratch
# line, it is never meant to actually run anything).
bindkey '^M' undefined
bindkey '^J' undefined

# explicit kill-whole-line bind for ^U: each request begins by sending ^U
# to discard any leftover buffer state from the previous request before
# writing the new line (see protocol note 3 above).
bindkey '^U' kill-whole-line

# send a line with null-byte at the end before and after completions are
# output (verbatim from capture.zsh's inner block).
null-line () {
    echo -E - $'\0'
}

# ZLE widget wrapper around complete-word that re-arms the one-shot
# compprefuncs/comppostfuncs arrays on EVERY invocation (see protocol
# notes 1-2 above — this is the daemon-specific addition capture.zsh does
# not need, since it only ever completes once per process).
jarvish-complete-word () {
    compprefuncs=( null-line )
    comppostfuncs=( null-line )
    zle complete-word
}
zle -N jarvish-complete-word
bindkey '^I' jarvish-complete-word

# never group stuff! (verbatim from capture.zsh's inner block)
zstyle ':completion:*' list-grouped false
# don't insert tab when attempting completion on empty line
zstyle ':completion:*' insert-tab false
# no list separator, this saves some stripping later on
zstyle ':completion:*' list-separator ''

# we use zparseopts
zmodload zsh/zutil

# override compadd (this our hook) — verbatim from capture.zsh's inner
# block (Valodim/zsh-capture-completion, MIT).
compadd () {

    # check if any of -O, -A or -D are given
    if [[ ${@[1,(i)(-|--)]} == *-(O|A|D)\ * ]]; then
        # if that is the case, just delegate and leave
        builtin compadd "$@"
        return $?
    fi

    # ok, this concerns us!
    # echo -E - got this: "$@"

    # be careful with namespacing here, we don't want to mess with stuff that
    # should be passed to compadd!
    typeset -a __hits __dscr __tmp

    # do we have a description parameter?
    # note we don't use zparseopts here because of combined option parameters
    # with arguments like -default- confuse it.
    if (( $@[(I)-d] )); then # kind of a hack, $+@[(r)-d] doesn't work because of line noise overload
        # next param after -d
        __tmp=${@[$[${@[(i)-d]}+1]]}
        # description can be given as an array parameter name, or inline () array
        if [[ $__tmp == \(* ]]; then
            eval "__dscr=$__tmp"
        else
            __dscr=( "${(@P)__tmp}" )
        fi
    fi

    # capture completions by injecting -A parameter into the compadd call.
    # this takes care of matching for us.
    builtin compadd -A __hits -D __dscr "$@"

    # JESUS CHRIST IT TOOK ME FOREVER TO FIGURE OUT THIS OPTION WAS SET AND WAS MESSING WITH MY SHIT HERE
    setopt localoptions norcexpandparam extendedglob

    # extract prefixes and suffixes from compadd call. we can't do zsh's cool
    # -r remove-func magic, but it's better than nothing.
    typeset -A apre hpre hsuf asuf
    zparseopts -E P:=apre p:=hpre S:=asuf s:=hsuf

    # append / to directories? we are only emulating -f in a half-assed way
    # here, but it's better than nothing.
    integer dirsuf=0
    # don't be fooled by -default- >.>
    if [[ -z $hsuf && "${${@//-default-/}% -# *}" == *-[[:alnum:]]#f* ]]; then
        dirsuf=1
    fi

    # just drop
    [[ -n $__hits ]] || return

    # this is the point where we have all matches in $__hits and all
    # descriptions in $__dscr!

    # display all matches
    local dsuf dscr
    for i in {1..$#__hits}; do

        # add a dir suffix?
        (( dirsuf )) && [[ -d $__hits[$i] ]] && dsuf=/ || dsuf=
        # description to be displayed afterwards
        (( $#__dscr >= $i )) && dscr=" -- ${${__dscr[$i]}##$__hits[$i] #}" || dscr=

        echo -E - $IPREFIX$apre$hpre$__hits[$i]$dsuf$hsuf$asuf$dscr

    done

}

# ready marker: jarvish's DaemonManager waits for this exact line on stdout
# before considering the daemon usable.
echo jarvish_daemon_ok
