use std::{collections::VecDeque, mem::size_of, path::Path};

use crate::{
    preview_file::{PreviewFile, PreviewFileIdentity},
    settings::LegacyEncoding,
    video,
};
use cursorpeek_core::protocol::DEFAULT_PREVIEW_CACHE_ENTRIES;

use super::{image, payload::PreviewResult, svg, text};

// The cache exists only for one contained worker session. The count cap follows QTTabBar's proven
// browsing working set, while the independent byte cap prevents 128 maximum-size decoded images
// from retaining hundreds of MiB. The entire cache is released when the idle worker retires.
const MAX_CACHE_BYTES: usize = 64 * 1024 * 1024;
// These versions make provider output semantics an explicit part of the key. Increment the
// relevant value if a future long-lived worker can switch implementation rules in-process.
const TEXT_PROVIDER_VERSION: u32 = 2;
const IMAGE_PROVIDER_VERSION: u32 = 2;
const IMAGE_ANIMATION_PROVIDER_VERSION: u32 = 1;
const SVG_PROVIDER_VERSION: u32 = 1;
const VIDEO_PROVIDER_VERSION: u32 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PreviewProvider {
    Svg,
    Text,
    Image,
    ImageAnimation,
    Video,
}

impl PreviewProvider {
    pub(super) fn for_path(path: &Path) -> Option<Self> {
        if svg::is_eligible_path(path) {
            Some(Self::Svg)
        } else if text::is_eligible_path(path) {
            Some(Self::Text)
        } else if image::is_eligible_path(path) {
            Some(Self::Image)
        } else if video::is_eligible_path(path) {
            Some(Self::Video)
        } else {
            None
        }
    }

    fn cache_key(self, legacy_encoding: &LegacyEncoding) -> PreviewProviderKey {
        match self {
            Self::Svg => PreviewProviderKey::Svg {
                version: SVG_PROVIDER_VERSION,
            },
            Self::Text => PreviewProviderKey::Text {
                version: TEXT_PROVIDER_VERSION,
                legacy_encoding: legacy_encoding.clone(),
            },
            Self::Image => PreviewProviderKey::Image {
                version: IMAGE_PROVIDER_VERSION,
            },
            Self::ImageAnimation => PreviewProviderKey::ImageAnimation {
                version: IMAGE_ANIMATION_PROVIDER_VERSION,
            },
            Self::Video => PreviewProviderKey::Video {
                version: VIDEO_PROVIDER_VERSION,
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PreviewProviderKey {
    Svg {
        version: u32,
    },
    Text {
        version: u32,
        legacy_encoding: LegacyEncoding,
    },
    Image {
        version: u32,
    },
    ImageAnimation {
        version: u32,
    },
    Video {
        version: u32,
    },
}

impl PreviewProviderKey {
    fn heap_bytes(&self) -> usize {
        match self {
            Self::Text {
                legacy_encoding: LegacyEncoding::Label(label),
                ..
            } => label.capacity(),
            Self::Svg { .. }
            | Self::Text { .. }
            | Self::Image { .. }
            | Self::ImageAnimation { .. }
            | Self::Video { .. } => 0,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PreviewCacheKey {
    file: PreviewFileIdentity,
    provider: PreviewProviderKey,
}

impl PreviewCacheKey {
    pub(super) fn new(
        file: &PreviewFile,
        provider: PreviewProvider,
        legacy_encoding: &LegacyEncoding,
    ) -> Self {
        Self {
            file: file.cache_identity(),
            provider: provider.cache_key(legacy_encoding),
        }
    }
}

struct CacheEntry {
    key: PreviewCacheKey,
    result: PreviewResult,
    retained_bytes: usize,
}

impl CacheEntry {
    fn new(key: PreviewCacheKey, result: PreviewResult) -> Option<Self> {
        let retained_bytes = size_of::<Self>()
            .checked_add(key.provider.heap_bytes())?
            .checked_add(result_heap_bytes(&result)?)?;
        Some(Self {
            key,
            result,
            retained_bytes,
        })
    }
}

fn result_heap_bytes(result: &PreviewResult) -> Option<usize> {
    match result {
        PreviewResult::Status(_) => None,
        PreviewResult::Text(preview) => preview
            .display_name
            .capacity()
            .checked_add(preview.encoding.capacity())
            .and_then(|length| length.checked_add(preview.text.capacity())),
        PreviewResult::Image(preview) => preview
            .display_name
            .capacity()
            .checked_add(preview.premultiplied_bgra.capacity())
            .and_then(|length| {
                preview
                    .animation_source
                    .as_ref()
                    .map_or(Some(length), |source| {
                        length.checked_add(source.path.capacity().checked_mul(size_of::<u16>())?)
                    })
            }),
        PreviewResult::ImageAnimation(preview) => preview
            .frame_delays_ms
            .capacity()
            .checked_mul(size_of::<u32>())?
            .checked_add(
                preview
                    .frames
                    .capacity()
                    .checked_mul(size_of::<Vec<u8>>())?,
            )?
            .checked_add(
                preview
                    .frames
                    .iter()
                    .try_fold(0_usize, |total, frame| total.checked_add(frame.capacity()))?,
            ),
        PreviewResult::Video(preview) => preview
            .display_name
            .capacity()
            .checked_add(preview.path.capacity().checked_mul(size_of::<u16>())?),
    }
}

pub(super) struct PreviewCache {
    entries: VecDeque<CacheEntry>,
    retained_bytes: usize,
    max_entries: usize,
    max_bytes: usize,
    #[cfg(test)]
    hit_count: usize,
}

impl Default for PreviewCache {
    fn default() -> Self {
        Self::with_entry_limit(DEFAULT_PREVIEW_CACHE_ENTRIES)
    }
}

impl PreviewCache {
    pub(super) fn with_entry_limit(max_entries: u16) -> Self {
        Self::with_limits(usize::from(max_entries), MAX_CACHE_BYTES)
    }

    fn with_limits(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: VecDeque::new(),
            retained_bytes: 0,
            max_entries,
            max_bytes,
            #[cfg(test)]
            hit_count: 0,
        }
    }

    pub(super) fn get(&mut self, key: &PreviewCacheKey) -> Option<PreviewResult> {
        let index = self.entries.iter().position(|entry| entry.key == *key)?;
        let entry = self
            .entries
            .remove(index)
            .expect("the located cache entry remains present");
        let result = entry.result.clone();
        self.entries.push_back(entry);
        #[cfg(test)]
        {
            self.hit_count += 1;
        }
        Some(result)
    }

    pub(super) fn insert(&mut self, key: PreviewCacheKey, result: PreviewResult) -> bool {
        let Some(entry) = CacheEntry::new(key, result) else {
            return false;
        };
        if self.max_entries == 0 || entry.retained_bytes > self.max_bytes {
            return false;
        }

        if let Some(index) = self
            .entries
            .iter()
            .position(|existing| existing.key == entry.key)
        {
            let replaced = self
                .entries
                .remove(index)
                .expect("the located cache entry remains present");
            self.retained_bytes -= replaced.retained_bytes;
        }

        while self.entries.len() >= self.max_entries
            || self
                .retained_bytes
                .checked_add(entry.retained_bytes)
                .is_none_or(|bytes| bytes > self.max_bytes)
        {
            let evicted = self
                .entries
                .pop_front()
                .expect("an individually bounded entry can fit an empty cache");
            self.retained_bytes -= evicted.retained_bytes;
        }

        self.retained_bytes += entry.retained_bytes;
        self.entries.push_back(entry);
        true
    }

    pub(super) fn entry_count(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub(super) const fn hit_count(&self) -> usize {
        self.hit_count
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entry_count()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CacheEntry, MAX_CACHE_BYTES, PreviewCache, PreviewCacheKey, PreviewProvider,
        PreviewProviderKey, TEXT_PROVIDER_VERSION,
    };
    use crate::{
        preview_file::PreviewFile,
        settings::LegacyEncoding,
        worker::payload::{PreviewResult, ResolverStatus, TextPreview},
    };
    use cursorpeek_core::protocol::DEFAULT_PREVIEW_CACHE_ENTRIES;
    use std::{
        env, fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT_TEST_FILE: AtomicU64 = AtomicU64::new(1);

    struct TestFile(PathBuf);

    impl Drop for TestFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn key(
        label: &str,
        provider: PreviewProvider,
        legacy_encoding: &LegacyEncoding,
    ) -> (TestFile, PreviewCacheKey) {
        let path = env::temp_dir().join(format!(
            "cursorpeek-cache-{}-{}-{label}.txt",
            std::process::id(),
            NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&path, label.as_bytes()).expect("the cache fixture should be written");
        let file = PreviewFile::open(&path).expect("the cache fixture should open");
        let key = PreviewCacheKey::new(&file, provider, legacy_encoding);
        (TestFile(path), key)
    }

    fn text_result(text: &str) -> PreviewResult {
        PreviewResult::Text(TextPreview {
            file_size: u64::try_from(text.len()).expect("the fixture length fits u64"),
            last_write_time: 133_000_000_000_000_000,
            linked_content: false,
            encoding_was_guessed: false,
            truncated: false,
            display_name: "sample.txt".to_owned(),
            encoding: "UTF-8".to_owned(),
            text: text.to_owned(),
        })
    }

    #[test]
    fn svg_selects_only_the_contained_vector_provider() {
        for path in [r"C:\logo.svg", r"C:\logo.SVG"] {
            assert_eq!(
                PreviewProvider::for_path(Path::new(path)),
                Some(PreviewProvider::Svg)
            );
        }
        assert_eq!(PreviewProvider::for_path(Path::new(r"C:\logo.svgz")), None);
    }

    #[test]
    fn key_separates_provider_policy_version_and_file_snapshot() {
        let (_first_file, automatic) = key("same", PreviewProvider::Text, &LegacyEncoding::Auto);
        let (_second_file, different_file) =
            key("same", PreviewProvider::Text, &LegacyEncoding::Auto);
        assert_ne!(automatic, different_file);

        let mut policy = automatic.clone();
        policy.provider = PreviewProviderKey::Text {
            version: 1,
            legacy_encoding: LegacyEncoding::Off,
        };
        assert_ne!(automatic, policy);

        let mut version = automatic.clone();
        version.provider = PreviewProviderKey::Text {
            version: TEXT_PROVIDER_VERSION + 1,
            legacy_encoding: LegacyEncoding::Auto,
        };
        assert_ne!(automatic, version);

        let mut image = automatic.clone();
        image.provider = PreviewProviderKey::Image { version: 1 };
        assert_ne!(automatic, image);
    }

    #[test]
    fn entry_cap_uses_least_recently_used_order() {
        let (_a_file, a) = key("a", PreviewProvider::Text, &LegacyEncoding::Auto);
        let (_b_file, b) = key("b", PreviewProvider::Text, &LegacyEncoding::Auto);
        let (_c_file, c) = key("c", PreviewProvider::Text, &LegacyEncoding::Auto);
        let mut cache = PreviewCache::with_limits(2, usize::MAX);

        assert!(cache.insert(a.clone(), text_result("a")));
        assert!(cache.insert(b.clone(), text_result("b")));
        assert_eq!(cache.get(&a), Some(text_result("a")));
        assert!(cache.insert(c.clone(), text_result("c")));

        assert_eq!(cache.get(&b), None);
        assert_eq!(cache.get(&a), Some(text_result("a")));
        assert_eq!(cache.get(&c), Some(text_result("c")));
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn default_cache_retains_a_bounded_folder_sized_working_set() {
        let entry_limit = usize::from(DEFAULT_PREVIEW_CACHE_ENTRIES);
        let mut fixtures = Vec::with_capacity(entry_limit + 1);
        let mut cache = PreviewCache::default();

        for index in 0..=entry_limit {
            let (file, key) = key(
                &format!("folder-entry-{index}"),
                PreviewProvider::Text,
                &LegacyEncoding::Auto,
            );
            fixtures.push((file, key));
        }

        for (_, key) in fixtures.iter().take(entry_limit) {
            assert!(cache.insert(key.clone(), text_result("cached folder entry")));
        }
        assert_eq!(cache.len(), entry_limit);

        let first = fixtures[0].1.clone();
        let second = fixtures[1].1.clone();
        assert_eq!(cache.get(&first), Some(text_result("cached folder entry")));
        assert!(cache.insert(
            fixtures[entry_limit].1.clone(),
            text_result("new folder entry")
        ));

        assert_eq!(cache.len(), entry_limit);
        assert_eq!(cache.get(&second), None);
        assert_eq!(
            cache.get(&first),
            Some(text_result("cached folder entry")),
            "a recent hit must survive count-based eviction"
        );
        assert_eq!(
            cache.get(&fixtures[entry_limit].1),
            Some(text_result("new folder entry"))
        );
    }

    #[test]
    fn default_cache_memory_contract_is_explicit() {
        let cache = PreviewCache::default();

        assert_eq!(cache.max_entries, 128);
        assert_eq!(cache.max_bytes, 64 * 1024 * 1024);
        assert_eq!(
            cache.max_entries,
            usize::from(DEFAULT_PREVIEW_CACHE_ENTRIES)
        );
        assert_eq!(cache.max_bytes, MAX_CACHE_BYTES);
    }

    #[test]
    fn configured_zero_entries_disables_caching_without_disabling_previews() {
        let (_file, key) = key("disabled", PreviewProvider::Text, &LegacyEncoding::Auto);
        let mut cache = PreviewCache::with_entry_limit(0);

        assert!(!cache.insert(key.clone(), text_result("not retained")));
        assert_eq!(cache.get(&key), None);
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn byte_cap_evicts_and_rejects_oversized_or_status_entries() {
        let (_a_file, a) = key("a", PreviewProvider::Text, &LegacyEncoding::Auto);
        let (_b_file, b) = key("b", PreviewProvider::Text, &LegacyEncoding::Auto);
        let a_result = text_result("first");
        let b_result = text_result("second");
        let a_bytes = CacheEntry::new(a.clone(), a_result.clone())
            .expect("a text result is cacheable")
            .retained_bytes;
        let b_bytes = CacheEntry::new(b.clone(), b_result.clone())
            .expect("a text result is cacheable")
            .retained_bytes;
        let mut cache = PreviewCache::with_limits(4, a_bytes.max(b_bytes));

        assert!(cache.insert(a.clone(), a_result));
        assert!(cache.insert(b.clone(), b_result.clone()));
        assert_eq!(cache.get(&a), None);
        assert_eq!(cache.get(&b), Some(b_result));

        let mut too_small = PreviewCache::with_limits(4, a_bytes - 1);
        assert!(!too_small.insert(a.clone(), text_result("first")));
        assert!(!too_small.insert(a, PreviewResult::Status(ResolverStatus::Unsupported)));
        assert_eq!(too_small.len(), 0);
    }
}
