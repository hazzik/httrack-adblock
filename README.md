# httrack-adblock

An [HTTrack](https://www.httrack.com/) plugin that applies Adblock Plus and uBlock Origin filter lists, such as EasyList and EasyPrivacy, while HTTrack mirrors a site. HTTrack does not download the ads and trackers that the lists block, and the saved pages do not contain the code that loads them.

## Usage

The repository does not contain filter lists. You can add a file with your own rules in the same syntax.

Pass the plugin to HTTrack with `-%W`, followed by the paths of the filter lists, separated by commas:

```sh
httrack https://example.com \
  -%W "/path/to/libhttrack_adblock.dylib,/path/to/easylist.txt,/path/to/my-rules.txt"
```

A path can also be a folder: the plugin then loads every `.txt` file in the folder and its subfolders.

On Windows, give the plugin path without the drive letter: HTTrack ends the plugin path at the first `:`.

`httrack --update` loads the plugin again with the same lists.

Set `HAB_DEBUG=1` to print, for each page, how many elements the plugin removed.

## What the plugin does

For each HTML page, before HTTrack parses it, the plugin removes:

- the elements that uBlock Origin HTML filters (`##^selector`, including `:has-text`) select;
- `<script>` and `<noscript>` blocks that load or mention a URL that the lists block;
- the elements that element-hiding rules (`##selector`) select, and `img`, `iframe`, `frame` and `embed` elements whose `src` the lists block.

For each link, the plugin tells HTTrack not to download it when the lists block it.

At the end of the mirror, the plugin prints how many links it blocked and how many elements it removed.

The plugin uses [Brave's `adblock` crate](https://github.com/brave/adblock-rust) to match URLs and element-hiding rules, and [`scraper`](https://github.com/rust-scraper/scraper) to parse the page as a browser does. When the plugin removes an element, `scraper` writes the whole page back out in its own markup style.

## Rules the plugin does not apply

- `##^responseheader(...)`, `##+js(...)` scriptlets and `$replace=` rules.
- HTML filters with procedural operators other than `:has-text`, such as `:upward()` or `:xpath()`.
- Selectors that `scraper` cannot parse, such as `:contains()` and `:-abp-has()`.

## Building

```sh
./build.sh
```

The plugin is in `target/release/`:

| Platform | File |
|---|---|
| macOS | `libhttrack_adblock.dylib` |
| Linux | `libhttrack_adblock.so` |
| Windows | `httrack_adblock.dll` |

## License

GNU Lesser General Public License version 3. See [LICENSE](LICENSE).
