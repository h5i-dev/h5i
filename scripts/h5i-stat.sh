#!/usr/bin/env bash
set -euo pipefail

REPO="h5i-dev/h5i"
MIN_VER=""

usage() {
  echo "Usage: $0 [-s|--since <version>] [-r|--repo <owner/repo>]"
  echo "  -s, --since   指定バージョン以降のみ集計 (例: v0.3.1 または 0.3.1)"
  echo "  -r, --repo    対象リポジトリ (デフォルト: h5i-dev/h5i)"
  exit 1
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -s|--since)
      MIN_VER="${2:-}"
      [[ -z "$MIN_VER" ]] && usage
      shift 2
      ;;
    -r|--repo)
      REPO="${2:-}"
      [[ -z "$REPO" ]] && usage
      shift 2
      ;;
    -h|--help)
      usage
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage
      ;;
  esac
done

# GitHub APIからデータを取得し、バージョンフィルタとアセット抽出を適用
DATA=$(gh api --paginate "repos/${REPO}/releases?per_page=100" | jq -s --arg min_ver "$MIN_VER" '
  def parse_ver(v):
    [v | ltrimstr("v") | split(".")[] | tonumber? // 0];

  [
    .[][] | .tag_name as $ver |
    select(
      if $min_ver == "" then true
      else (parse_ver($ver) >= parse_ver($min_ver))
      end
    ) |
    .assets[] |
    select(.name | test("\\.(tar\\.gz|zip|whl)$")) |
    (
      if (.name | endswith(".whl")) then
        { bin: "h5i_agent(whl)", os: "python" }
      elif (.name | test("^(?<b_name>.+)-v[0-9]+\\.[0-9]+\\.[0-9]+-(?<t_os>.+)\\.(tar\\.gz|zip)$")) then
        (.name | capture("^(?<b_name>.+)-v[0-9]+\\.[0-9]+\\.[0-9]+-(?<t_os>.+)\\.(tar\\.gz|zip)$")) |
        {
          bin: .b_name,
          os: (
            if .t_os == "aarch64-apple-darwin" then "macos-arm64"
            elif .t_os == "aarch64-unknown-linux-musl" then "linux-arm64"
            elif .t_os == "x86_64-unknown-linux-musl" then "linux-x64"
            elif .t_os == "x86_64-pc-windows-msvc" then "win-x64"
            else .t_os end
          )
        }
      else
        { bin: "other", os: "other" }
      end
    ) as $parsed |
    {
      ver: $ver,
      bin: $parsed.bin,
      os: $parsed.os,
      dl: .download_count
    }
  ]
')

FILTER_LABEL="${MIN_VER:+ (>= ${MIN_VER})}"

echo "======================================================================"
echo " 1. バイナリ種類別 × OS 合計ダウンロード数${FILTER_LABEL}"
echo "======================================================================"
echo "$DATA" | jq -r '
  ["BINARY", "LINUX-X64", "LINUX-ARM64", "MACOS-ARM64", "WIN-X64", "PYTHON", "TOTAL"],
  ["------", "---------", "-----------", "-----------", "-------", "------", "-----"],
  (
    group_by(.bin)[] |
    .[0].bin as $b |
    (map(select(.os == "linux-x64").dl) | add // 0) as $lx64 |
    (map(select(.os == "linux-arm64").dl) | add // 0) as $larm |
    (map(select(.os == "macos-arm64").dl) | add // 0) as $mac |
    (map(select(.os == "win-x64").dl) | add // 0) as $win |
    (map(select(.os == "python").dl) | add // 0) as $py |
    (map(.dl) | add // 0) as $tot |
    [$b, $lx64, $larm, $mac, $win, $py, $tot]
  ),
  ["------", "---------", "-----------", "-----------", "-------", "------", "-----"],
  (
    ["TOTAL",
     (map(select(.os == "linux-x64").dl) | add // 0),
     (map(select(.os == "linux-arm64").dl) | add // 0),
     (map(select(.os == "macos-arm64").dl) | add // 0),
     (map(select(.os == "win-x64").dl) | add // 0),
     (map(select(.os == "python").dl) | add // 0),
     (map(.dl) | add // 0)
    ]
  )
  | @tsv
' | column -t -s $'\t'

echo ""
echo "======================================================================"
echo " 2. バージョン別 × バイナリ種類 合計ダウンロード数${FILTER_LABEL}"
echo "======================================================================"
echo "$DATA" | jq -r '
  ["VERSION", "h5i(core)", "h5i-websec", "h5i-recon", "h5i-test", "browser-light", "whl", "TOTAL"],
  ["-------", "---------", "----------", "---------", "--------", "-------------", "---", "-----"],
  (
    reduce .[] as $item ({vers: [], map: {}};
      if (.vers | index($item.ver)) == null then .vers += [$item.ver] else . end |
      .map[$item.ver] += [$item]
    ) |
    .vers[] as $v | .map[$v] |
    (map(select(.bin == "h5i").dl) | add // 0) as $core |
    (map(select(.bin == "h5i-websec").dl) | add // 0) as $websec |
    (map(select(.bin == "h5i-recon").dl) | add // 0) as $recon |
    (map(select(.bin == "h5i-test").dl) | add // 0) as $test |
    (map(select(.bin == "h5i-browser-light").dl) | add // 0) as $light |
    (map(select(.bin == "h5i_agent(whl)").dl) | add // 0) as $whl |
    (map(.dl) | add // 0) as $tot |
    [$v, $core, $websec, $recon, $test, $light, $whl, $tot]
  ),
  ["-------", "---------", "----------", "---------", "--------", "-------------", "---", "-----"],
  (
    ["TOTAL",
     (map(select(.bin == "h5i").dl) | add // 0),
     (map(select(.bin == "h5i-websec").dl) | add // 0),
     (map(select(.bin == "h5i-recon").dl) | add // 0),
     (map(select(.bin == "h5i-test").dl) | add // 0),
     (map(select(.bin == "h5i-browser-light").dl) | add // 0),
     (map(select(.bin == "h5i_agent(whl)").dl) | add // 0),
     (map(.dl) | add // 0)
    ]
  )
  | @tsv
' | column -t -s $'\t'

echo ""
echo "======================================================================"
echo " 3. 詳細内訳 (バージョン × バイナリ × OS)${FILTER_LABEL}"
echo "======================================================================"
echo "$DATA" | jq -r '
  ["VERSION", "BINARY", "LINUX-X64", "LINUX-ARM64", "MACOS-ARM64", "WIN-X64", "PY-WHL", "SUBTOTAL"],
  ["-------", "------", "---------", "-----------", "-----------", "-------", "------", "--------"],
  (
    reduce .[] as $item ({keys: [], map: {}};
      ($item.ver + "\u0000" + $item.bin) as $k |
      if (.keys | index($k)) == null then .keys += [$k] else . end |
      .map[$k] += [$item]
    ) |
    .keys[] as $k | .map[$k] |
    .[0].ver as $v | .[0].bin as $b |
    (map(select(.os == "linux-x64").dl) | add // 0) as $lx64 |
    (map(select(.os == "linux-arm64").dl) | add // 0) as $larm |
    (map(select(.os == "macos-arm64").dl) | add // 0) as $mac |
    (map(select(.os == "win-x64").dl) | add // 0) as $win |
    (map(select(.os == "python").dl) | add // 0) as $py |
    (map(.dl) | add // 0) as $tot |
    [$v, $b, $lx64, $larm, $mac, $win, $py, $tot]
  )
  | @tsv
' | column -t -s $'\t'
