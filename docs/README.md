# MMD Viewer ドキュメント

MMD Viewer（実行ファイル名 `mmd-viewer`）は、MikuMikuDance（以下 MMD）の PMX / PMD モデルと VMD モーションを扱う、Windows 向けのアニメーションエディタです。このディレクトリには、コードの構造と挙動の説明をまとめています。

記述はすべて、現在のコードに合わせています。MMD 本体と数値で照合していない挙動は、その旨を明記しています。

## 読む順番

初めて読む場合は、上から順に読むと全体がつかめます。

| 文書 | 内容 |
| --- | --- |
| [architecture/overview.md](architecture/overview.md) | 全体像。クレート構成図、依存関係、スレッド構成、データの持ち方 |
| [architecture/workflow.md](architecture/workflow.md) | 実行時の処理の流れ。起動、読み込み、再生 1 フレームの処理を図で説明 |
| [architecture/coordinates.md](architecture/coordinates.md) | 座標系。モデル空間とクリップ空間の約束事 |
| [architecture/animation.md](architecture/animation.md) | アニメーション評価。キーフレーム補間、モーフ、付与、IK、スキニング |
| [architecture/physics.md](architecture/physics.md) | 物理演算。Bullet の設定、剛体とジョイント、ボーンへの書き戻し |
| [architecture/rendering.md](architecture/rendering.md) | 描画。描画パス、MMD 互換のシェーディング式、透過の扱い |
| [development/building.md](development/building.md) | ビルド方法。必要なツール、コマンド、ログ |
| [development/workflow.md](development/workflow.md) | 開発の進め方。作業手順、必須チェック、CI |

## 設計判断の記録（ADR）

設計上の大きな判断は ADR（Architecture Decision Record）として残しています。

| ADR | 判断 | 状態 |
| --- | --- | --- |
| [0001](adr/0001-phase-0-foundation.md) | Phase 0 の土台（ワークスペース構成、GPU 初期化、CI） | 採用。座標系の部分は 0002 で置き換え |
| [0002](adr/0002-left-handed-model-space.md) | 実行時のモデル空間を、PMX と同じ左手系にする | 採用 |
| [0003](adr/0003-explicit-binary.md) | PMX / PMD / VMD を明示的なカーソルで読む | 採用 |
| [0004](adr/0004-bullet-physics-backend.md) | 物理ソルバーに Bullet 3.25 を使う | 採用 |

## 用語

| 用語 | 意味 |
| --- | --- |
| モデル（PMX / PMD） | 頂点、材質、ボーン、モーフ、剛体、ジョイントを持つキャラクターのファイル |
| モーション（VMD） | ボーン、モーフ、カメラ、ライト、IK の ON/OFF のキーフレームを持つファイル |
| フレーム | MMD の時間の単位。1 秒 = 30 フレーム。内部では整数で持つ |
| 評価（evaluate） | あるフレームでの全ボーンの姿勢とモーフの重みを計算すること |
| スキン行列 | ボーンの「現在のワールド変換 × 初期姿勢の逆行列」。頂点を動かすのに使う |
| キネマティック剛体 | ボーンに追従するだけの剛体（PMX の剛体タイプ 0） |
| 動的剛体 | 物理で動き、ボーンを動かす剛体（タイプ 1 と 2） |
| トゥーン | 陰影の色を決める縦長のテクスチャ。上が明るく下が暗い |
| スフィアマップ | 法線から引く疑似的な反射テクスチャ。加算または乗算する |
