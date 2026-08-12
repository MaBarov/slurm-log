struct IndexedDirectory {
    key: Expanded,
    ancestors: Vec<Expanded>,
    depth: usize,
    files: Vec<usize>,
}

#[derive(Default)]
struct IndexedBank {
    visible: usize,
    directories: Vec<IndexedDirectory>,
    root_files: Vec<usize>,
}

struct ClusterBankIndex {
    name: String,
    banks: Vec<IndexedBank>,
}

struct BankIndex {
    clusters: Vec<ClusterBankIndex>,
    search: Vec<String>,
}

impl BankIndex {
    fn new<'a>(
        banks: &[LoadedBank],
        scripts: &[Script],
        clusters: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let clusters = clusters
            .into_iter()
            .map(|cluster| ClusterBankIndex {
                name: cluster.to_string(),
                banks: banks
                    .iter()
                    .enumerate()
                    .map(|(bank, range)| index_bank(bank, range, scripts, cluster))
                    .collect(),
            })
            .collect();
        let search = scripts
            .iter()
            .map(|script| {
                format!("{}/{}", script.bank, script.relative.display()).to_lowercase()
            })
            .collect();
        Self { clusters, search }
    }

    fn cluster(&self, name: &str) -> &ClusterBankIndex {
        self.clusters
            .iter()
            .find(|cluster| cluster.name == name)
            .expect("configured cluster has a bank index")
    }

    fn visible_scripts(&self, cluster: &str) -> usize {
        self.cluster(cluster)
            .banks
            .iter()
            .map(|bank| bank.visible)
            .sum()
    }

    fn rows(
        &self,
        scripts: &[Script],
        expanded: &HashSet<Expanded>,
        query: &str,
        cluster: &str,
    ) -> Vec<BankRow> {
        if !query.is_empty() {
            let needle = query.to_lowercase();
            return self.search
                .iter()
                .enumerate()
                .filter(|(index, text)| {
                    supports_cluster(&scripts[*index], cluster) && text.contains(&needle)
                })
                .map(|(index, _)| BankRow::File(index, 1))
                .collect();
        }
        let mut rows = Vec::new();
        for (bank_index, bank) in self.cluster(cluster).banks.iter().enumerate() {
            if bank.visible == 0 {
                continue;
            }
            let bank_open = expanded.contains(&Expanded::Bank(bank_index));
            rows.push(BankRow::Bank(bank_index, bank_open, bank.visible));
            if !bank_open {
                continue;
            }
            for directory in &bank.directories {
                let visible = directory
                    .ancestors
                    .iter()
                    .all(|ancestor| expanded.contains(ancestor));
                if !visible {
                    continue;
                }
                let open = expanded.contains(&directory.key);
                let Expanded::Directory(_, path) = &directory.key else {
                    unreachable!();
                };
                rows.push(BankRow::Directory(
                    bank_index,
                    path.clone(),
                    directory.depth,
                    open,
                ));
                if open {
                    rows.extend(
                        directory
                            .files
                            .iter()
                            .map(|&index| BankRow::File(index, directory.depth)),
                    );
                }
            }
            rows.extend(
                bank.root_files
                    .iter()
                    .map(|&index| BankRow::File(index, 1)),
            );
        }
        rows
    }
}

fn index_bank(bank: usize, range: &LoadedBank, scripts: &[Script], cluster: &str) -> IndexedBank {
    let mut indexed = IndexedBank::default();
    let mut directories: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
    for (index, script) in scripts
        .iter()
        .enumerate()
        .take(range.last)
        .skip(range.first)
    {
        if !supports_cluster(script, cluster) {
            continue;
        }
        indexed.visible += 1;
        let parent = script.relative.parent().unwrap_or_else(|| Path::new(""));
        if parent.as_os_str().is_empty() {
            indexed.root_files.push(index);
            continue;
        }
        directories.entry(parent.to_path_buf()).or_default().push(index);
        for ancestor in parent.ancestors().skip(1) {
            if !ancestor.as_os_str().is_empty() {
                directories.entry(ancestor.to_path_buf()).or_default();
            }
        }
    }
    indexed.directories = directories
        .into_iter()
        .map(|(path, files)| {
            let ancestors = path
                .ancestors()
                .skip(1)
                .filter(|path| !path.as_os_str().is_empty())
                .map(|path| Expanded::Directory(bank, path.to_path_buf()))
                .collect();
            let depth = path.components().count() + 1;
            IndexedDirectory {
                key: Expanded::Directory(bank, path),
                ancestors,
                depth,
                files,
            }
        })
        .collect();
    indexed
}

#[cfg(test)]
fn rows(
    banks: &[LoadedBank],
    scripts: &[Script],
    expanded: &HashSet<Expanded>,
    query: &str,
    cluster: &str,
) -> Vec<BankRow> {
    BankIndex::new(banks, scripts, [cluster]).rows(scripts, expanded, query, cluster)
}
