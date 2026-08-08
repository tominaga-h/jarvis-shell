use super::*;

/// [`ZshDaemon::read_framed_response`] の結果。
pub(crate) enum FramedRead {
    /// 2つのセンチネルに挟まれたフレームを正常に読み取れた。
    Frame(String),
    /// `deadline` までにセンチネルが2個揃わなかった（純粋なタイムアウト、
    /// またはセンチネルが1個も来ないまま `deadline` に達した場合を含む）。
    Timeout,
    /// 応答バッファが [`MAX_RESPONSE_BYTES`] を超えた。プロトコル
    /// desync 相当として扱う——グレースの対象外。
    BufferOverflow,
}

/// [`ZshDaemon::read_framed_response`] が `request()`/`drain_pending_frame`
/// の呼び出しをまたいで持ち越す部分読み取り状態。
///
/// `ZshDaemon::partial_read` フィールドのドキュメント参照——1つの論理
/// フレームの開始・終了センチネルが異なる呼び出し（元のリクエストと、
/// それに続くドレイン呼び出し）にまたがって届くケースに対応するため、
/// バッファとトグルカウントを呼び出し間で保持する。
#[derive(Default)]
pub(crate) struct PartialRead {
    /// これまでに読み取った生バイト列（フレームが完成する、または
    /// バッファ上限超過/desync でリセットされるまで蓄積し続ける）。
    buf: Vec<u8>,
    /// これまでに検出したセンチネルバイトの個数（0, 1, または 2）。
    toggles: u8,
    /// 最初のセンチネル直後のオフセット（`buf` 内、フレーム本体の開始位置）。
    frame_start: Option<usize>,
}

impl PartialRead {
    /// フレームが完成した（`toggles == 2`）ときに、次のリクエストに
    /// 備えて状態を空にリセットする。
    fn reset(&mut self) {
        self.buf.clear();
        self.toggles = 0;
        self.frame_start = None;
    }
}
impl ZshDaemon {
    /// PTY master から `timeout` 予算内でセンチネル2個に挟まれた1フレーム分
    /// を読み取る（[`request`](Self::request) / [`drain_pending_frame`]
    /// 共通のフレーミングロジックを切り出したもの）。バッファ上限
    /// （[`MAX_RESPONSE_BYTES`]）超過はタイムアウトより優先して検知する。
    ///
    /// # 呼び出しをまたぐ状態の持ち越し（[`PartialRead`] 参照）
    /// 読み取り状態（`buf`/`toggles`/`frame_start`）は `self.partial_read`
    /// に保持し、呼び出しごとにリセットしない。開始センチネルが前回の
    /// 呼び出し（元のリクエスト）内で既に届いていた場合、この呼び出しは
    /// 「終了センチネルだけを待てばよい」状態から再開する——`toggles == 1`
    /// のまま呼ばれることも正常なケースであり、その場合でも新しく読めた
    /// バイト内で NUL が1個見つかれば `toggles` が2に達してフレーム完成と
    /// 判定できる。フレームが完成した時点（`toggles == 2`）で
    /// [`PartialRead::reset`] を呼び、次の論理フレーム用にまっさらな状態へ
    /// 戻す（バッファ上限超過時も同様——desync 扱いで kill されるため
    /// どのみち次のフレームは存在しないが、防御的にリセットしておく）。
    pub(super) fn read_framed_response(&mut self, timeout: Duration) -> FramedRead {
        let deadline = Instant::now() + timeout;

        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let step = remaining.min(Duration::from_millis(200));
            match read_available(&mut self.master, step) {
                Some(chunk) if !chunk.is_empty() => {
                    let base = self.partial_read.buf.len();
                    self.partial_read.buf.extend_from_slice(&chunk);
                    // 新しく読めたバイト範囲内で NUL をスキャンする。
                    let mut idx = base;
                    while idx < self.partial_read.buf.len() {
                        if self.partial_read.buf[idx] == SENTINEL_BYTE {
                            self.partial_read.toggles += 1;
                            if self.partial_read.toggles == 1 {
                                self.partial_read.frame_start = Some(idx + 1);
                            } else if self.partial_read.toggles == 2 {
                                let start = self
                                    .partial_read
                                    .frame_start
                                    .expect("toggles==2 implies frame_start was set at toggle 1");
                                let frame =
                                    String::from_utf8_lossy(&self.partial_read.buf[start..idx])
                                        .into_owned();
                                self.partial_read.reset();
                                return FramedRead::Frame(frame);
                            }
                        }
                        idx += 1;
                    }
                    // 上限超過はプロトコル desync 相当として扱い、
                    // タイムアウトを待たず即座に打ち切る。
                    if self.partial_read.buf.len() > MAX_RESPONSE_BYTES {
                        self.partial_read.reset();
                        return FramedRead::BufferOverflow;
                    }
                }
                _ => continue,
            }
        }

        FramedRead::Timeout
    }
}
