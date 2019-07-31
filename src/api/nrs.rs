// Copyright 2019 MaidSafe.net limited.
//
// This SAFE Network Software is licensed to you under The General Public License (GPL), version 3.
// Unless required by applicable law or agreed to in writing, the SAFE Network Software distributed
// under the GPL Licence is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
// KIND, either express or implied. Please review the Licences for the specific language governing
// permissions and limitations relating to use of the SAFE Network Software.

use super::constants::{CONTENT_ADDED_SIGN, CONTENT_DELETED_SIGN};
use super::helpers::{gen_timestamp_secs, get_subnames_host_and_path};
use super::nrs_map::NrsMap;
use super::xorurl::{SafeContentType, SafeDataType};
use super::{Error, ResultReturn, Safe, SafeApp, XorUrl, XorUrlEncoder};
use log::{debug, info, warn};
use safe_nd::XorName;
use std::collections::BTreeMap;
use tiny_keccak::sha3_256;

// Type tag to use for the NrsMapContainer stored on AppendOnlyData
const NRS_MAP_TYPE_TAG: u64 = 1_500;

const ERROR_MSG_NO_NRS_MAP_FOUND: &str = "No NRS Map found at this address";

// Raw data stored in the SAFE native data type for a NRS Map Container
type NrsMapRawData = Vec<(Vec<u8>, Vec<u8>)>;

// List of public names uploaded with details if they were added, updated or deleted from NrsMaps
type ProcessedEntries = BTreeMap<String, (String, String)>;

#[allow(dead_code)]
impl Safe {
    pub fn parse_url(&self, url: &str) -> ResultReturn<XorUrlEncoder> {
        debug!("Attempting to decode url: {}", url);
        XorUrlEncoder::from_url(url).or_else(|err| {
            info!(
                "Falling back to NRS. XorUrl decoding failed with: {:?}",
                err
            );

            let (sub_names, host_str, path) = get_subnames_host_and_path(url)?;
            let hashed_host = xorname_from_nrs_string(&host_str)?;

            let encoded_xor = XorUrlEncoder::new(
                hashed_host,
                NRS_MAP_TYPE_TAG,
                SafeDataType::PublishedSeqAppendOnlyData,
                SafeContentType::NrsMapContainer,
                Some(&path),
                Some(sub_names),
            );

            Ok(encoded_xor)
        })
    }

    pub fn nrs_map_container_add(
        &mut self,
        name: &str,
        destination: Option<&str>,
        default: bool,
        dry_run: bool,
    ) -> ResultReturn<(u64, XorUrl, ProcessedEntries, NrsMap)> {
        info!("Adding to NRS map...");
        // GET current NRS map from name's TLD
        let xorurl_encoder = self.parse_url(&sanitised_nrs_url(name))?;
        let xorurl = xorurl_encoder.to_string("")?;
        let (version, mut nrs_map) = self.nrs_map_container_get_latest(&xorurl)?;
        debug!("NRS, Existing data: {:?}", nrs_map);

        let link = nrs_map.nrs_map_update_or_create_data(name, destination, default)?;
        let mut processed_entries = ProcessedEntries::new();
        processed_entries.insert(
            name.to_string(),
            (CONTENT_ADDED_SIGN.to_string(), link.to_string()),
        );

        let nrs_map_raw_data = gen_nrs_map_raw_data(&nrs_map)?;
        debug!("The new NRS Map: {:?}", nrs_map);

        if !dry_run {
            // Append new version of the NrsMap in the Published AppendOnlyData (NRS Map Container)
            self.safe_app.append_seq_append_only_data(
                nrs_map_raw_data,
                version + 1,
                xorurl_encoder.xorname(),
                xorurl_encoder.type_tag(),
            )?;
        }

        Ok((version + 1, xorurl, processed_entries, nrs_map))
    }

    /// # Create a NrsMapContainer.
    ///
    /// ## Example
    ///
    /// ```rust
    /// # use rand::distributions::Alphanumeric;
    /// # use rand::{thread_rng, Rng};
    /// # use unwrap::unwrap;
    /// # use safe_cli::Safe;
    /// # let mut safe = Safe::new("base32z".to_string());
    /// # safe.connect("", Some("fake-credentials")).unwrap();
    /// let rand_string: String = thread_rng().sample_iter(&Alphanumeric).take(15).collect();
    /// let (xorurl, _processed_entries, nrs_map_container) = safe.nrs_map_container_create(&rand_string, Some("safe://somewhere"), true, false).unwrap();
    /// assert!(xorurl.contains("safe://"))
    /// ```
    pub fn nrs_map_container_create(
        &mut self,
        name: &str,
        destination: Option<&str>,
        default: bool,
        dry_run: bool,
    ) -> ResultReturn<(XorUrl, ProcessedEntries, NrsMap)> {
        info!("Creating an NRS map");
        let nrs_url = sanitised_nrs_url(name);
        if self.nrs_map_container_get_latest(&nrs_url).is_ok() {
            Err(Error::ContentError(
                "NRS name already exists. Please use 'nrs add' command to add sub names to it"
                    .to_string(),
            ))
        } else {
            let mut nrs_map = NrsMap::default();
            let link = nrs_map.nrs_map_update_or_create_data(&name, destination, default)?;
            let mut processed_entries = ProcessedEntries::new();
            processed_entries.insert(
                name.to_string(),
                (CONTENT_ADDED_SIGN.to_string(), link.to_string()),
            );

            let nrs_map_raw_data = gen_nrs_map_raw_data(&nrs_map)?;

            if dry_run {
                Ok(("".to_string(), processed_entries, nrs_map))
            } else {
                let (_, public_name, _) = get_subnames_host_and_path(&nrs_url)?;
                let nrs_xorname = xorname_from_nrs_string(&public_name)?;
                debug!(
                    "XorName for \"{:?}\" is \"{:?}\"",
                    &public_name, &nrs_xorname
                );

                // Store the NrsMapContainer in a Published AppendOnlyData
                let xorname = self.safe_app.put_seq_append_only_data(
                    nrs_map_raw_data,
                    Some(nrs_xorname),
                    NRS_MAP_TYPE_TAG,
                    None,
                )?;

                let xorurl = XorUrlEncoder::encode(
                    xorname,
                    NRS_MAP_TYPE_TAG,
                    SafeDataType::PublishedSeqAppendOnlyData,
                    SafeContentType::NrsMapContainer,
                    None,
                    None,
                    &self.xorurl_base,
                )?;

                Ok((xorurl, processed_entries, nrs_map))
            }
        }
    }

    pub fn nrs_map_container_remove(
        &mut self,
        name: &str,
        dry_run: bool,
    ) -> ResultReturn<(u64, XorUrl, ProcessedEntries, NrsMap)> {
        info!("Removing from NRS map...");
        // GET current NRS map from &name TLD
        let xorurl_encoder = self.parse_url(&sanitised_nrs_url(name))?;
        let xorurl = xorurl_encoder.to_string("")?;
        let (version, mut nrs_map) = self.nrs_map_container_get_latest(&xorurl)?;
        debug!("NRS, Existing data: {:?}", nrs_map);

        let removed_link = nrs_map.nrs_map_remove_subname(name)?;
        let mut processed_entries = ProcessedEntries::new();
        processed_entries.insert(
            name.to_string(),
            (CONTENT_DELETED_SIGN.to_string(), removed_link),
        );
        let nrs_map_raw_data = gen_nrs_map_raw_data(&nrs_map)?;

        debug!("The new NRS Map: {:?}", nrs_map);
        if !dry_run {
            // Append new version of the NrsMap in the Published AppendOnlyData (NRS Map Container)
            self.safe_app.append_seq_append_only_data(
                nrs_map_raw_data,
                version + 1,
                xorurl_encoder.xorname(),
                xorurl_encoder.type_tag(),
            )?;
        }

        Ok((version + 1, xorurl, processed_entries, nrs_map))
    }

    /// # Fetch an existing NrsMapContainer.
    ///
    /// ## Example
    ///
    /// ```rust
    /// # use safe_cli::Safe;
    /// # use rand::distributions::Alphanumeric;
    /// # use rand::{thread_rng, Rng};
    /// # let mut safe = Safe::new("base32z".to_string());
    /// # safe.connect("", Some("fake-credentials")).unwrap();
    /// let rand_string: String = thread_rng().sample_iter(&Alphanumeric).take(15).collect();
    /// let (xorurl, _processed_entries, _nrs_map) = safe.nrs_map_container_create(&rand_string, Some("somewhere"), true, false).unwrap();
    /// let (version, nrs_map_container) = safe.nrs_map_container_get_latest(&xorurl).unwrap();
    /// assert_eq!(version, 1);
    /// assert_eq!(nrs_map_container.get_default_link().unwrap(), "somewhere");
    /// ```
    pub fn nrs_map_container_get_latest(&self, url: &str) -> ResultReturn<(u64, NrsMap)> {
        debug!("Getting latest resolvable map container from: {:?}", url);

        let xorurl_encoder = self.parse_url(url)?;
        match self
            .safe_app
            .get_latest_seq_append_only_data(xorurl_encoder.xorname(), NRS_MAP_TYPE_TAG)
        {
            Ok((version, (_key, value))) => {
                debug!("Nrs map retrieved.... v{:?}, value {:?} ", &version, &value);
                // TODO: use RDF format and deserialise it
                let nrs_map = serde_json::from_str(&String::from_utf8_lossy(&value.as_slice()))
                    .map_err(|err| {
                        Error::ContentError(format!(
                            "Couldn't deserialise the NrsMap stored in the NrsContainer: {:?}",
                            err
                        ))
                    })?;
                Ok((version, nrs_map))
            }
            Err(Error::EmptyContent(_)) => {
                warn!("Nrs container found at {:?} was empty", &url);
                Ok((0, NrsMap::default()))
            }
            Err(Error::ContentNotFound(_)) => Err(Error::ContentNotFound(
                ERROR_MSG_NO_NRS_MAP_FOUND.to_string(),
            )),
            Err(err) => Err(Error::NetDataError(format!(
                "Failed to get current version: {}",
                err
            ))),
        }
    }
}

fn xorname_from_nrs_string(name: &str) -> ResultReturn<XorName> {
    let vec_hash = sha3_256(&name.to_string().into_bytes());
    let xorname = XorName(vec_hash);
    debug!("Resulting XorName for NRS \"{}\" is: {}", name, xorname);
    Ok(xorname)
}

fn sanitised_nrs_url(name: &str) -> String {
    // FIXME: make sure we remove the starting 'safe://'
    format!("safe://{}", name.replace("safe://", ""))
}

fn gen_nrs_map_raw_data(nrs_map: &NrsMap) -> ResultReturn<NrsMapRawData> {
    // The NrsMapContainer is an AppendOnlyData where each NRS Map version is an entry containing
    // the timestamp as the entry's key, and the serialised NrsMap as the entry's value
    // TODO: use RDF format
    let serialised_nrs_map = serde_json::to_string(nrs_map).map_err(|err| {
        Error::Unexpected(format!(
            "Couldn't serialise the NrsMap generated: {:?}",
            err
        ))
    })?;
    let now = gen_timestamp_secs();

    Ok(vec![(
        now.into_bytes().to_vec(),
        serialised_nrs_map.as_bytes().to_vec(),
    )])
}

// Unit Tests

#[test]
fn test_nrs_map_container_create() {
    use super::constants::FAKE_RDF_PREDICATE_LINK;
    use super::nrs_map::DefaultRdf;
    use rand::distributions::Alphanumeric;
    use rand::{thread_rng, Rng};
    use unwrap::unwrap;

    let site_name: String = thread_rng().sample_iter(&Alphanumeric).take(15).collect();

    let mut safe = Safe::new("base32z".to_string());
    safe.connect("", Some("fake-credentials")).unwrap();

    let nrs_xorname = xorname_from_nrs_string(&site_name).unwrap();

    let (xor_url, _entries, nrs_map) =
        unwrap!(safe.nrs_map_container_create(&site_name, Some("safe://top_xorurl"), true, false));
    assert_eq!(nrs_map.sub_names_map.len(), 0);

    if let DefaultRdf::OtherRdf(def_data) = &nrs_map.default {
        assert_eq!(
            *def_data.get(FAKE_RDF_PREDICATE_LINK).unwrap(),
            "safe://top_xorurl".to_string()
        );
        assert_eq!(
            nrs_map.get_default().unwrap(),
            &DefaultRdf::OtherRdf(def_data.clone())
        );
    } else {
        panic!("No default definition map found...")
    }

    let decoder = XorUrlEncoder::from_url(&xor_url).unwrap();
    assert_eq!(nrs_xorname, decoder.xorname())
}
