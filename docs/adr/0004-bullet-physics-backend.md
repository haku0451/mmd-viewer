# ADR 0004: 物理ソルバーに Bullet 3.25 を使う

## 状態

採用

## 背景

MMD 本体は、物理演算に Bullet Physics を使っている。配布モデルの剛体とジョイントの数値は、Bullet の挙動を前提に調整されている。特に効いてくるのは次の点である。

- 減衰の式と追加減衰
- `btGeneric6DofSpringConstraint` によるジョイントとバネ
- 逐次インパルス法のソルバーが完全には収束しないことによる、ロックしたジョイントのたわみ（長い髪が背中に沿って垂れるのはこれによる）
- 衝突グループとマスクの判定

## 決定

- 物理ソルバーに **Bullet 3.25** を使う。ソースは `vendor/bullet3` の Git サブモジュールとして持つ。
- `mmd-physics` の `build.rs` が Bullet を C++17・最適化ありでビルドし、CXX で Rust とつなぐ（`src/bullet.rs`、`src/bullet_world.cpp`、`src/bullet_world.h`）。
- Bullet のワールドは C++ 側の `World` クラスに閉じ込め、Rust からは剛体の追加、ジョイントの追加、ステップ、姿勢の読み書き、速度の取得だけを呼ぶ。
- 時間の進め方、キネマティック剛体の補間、ボーンへの書き戻し、静止判定は Rust 側（`src/sim.rs`）に置く。
- Bullet ワールドはモデルごとのポーズスレッドの中で作り、そのスレッドから出さない。これにより、C++ の型を別スレッドへ送るための `unsafe` を書かずに済む。

## 結果

- 剛体とジョイントの挙動が、MMD と同じ Bullet の挙動になる。設定は MMD 互換の実装（MMDAgent、saba、babylon-mmd）に合わせ、反復回数と刻み幅は手元のモデルで調整した（[docs/architecture/physics.md](../architecture/physics.md)）。
- ビルドに C++ コンパイラ（MSVC）と、サブモジュールの取得が必要になる。CI はサブモジュールごとチェックアウトする。
- FFI の境界ができる。境界のコードは CXX が生成し、手書きの `unsafe` は使わない。
- 物理エンジンを差し替えても同じ結果になるとは限らない（`AGENTS.md` の方針）。別のエンジンを追加する場合は、Bullet との比較を docs に残す。
