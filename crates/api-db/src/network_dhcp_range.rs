/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Database operations for network prefixes.
//!
//! Explicit result columns keep this table's queries working across column
//! additions. Cached wildcard statements otherwise fail with PostgreSQL's
//! "cached plan must not change result type".

//! IPv4 DHCP allocation intervals, changed under the allocator's segment lock.

use std::net::IpAddr;

use carbide_uuid::network::{NetworkPrefixId, NetworkSegmentId};
use ipnetwork::IpNetwork;
use model::network_segment::{NetworkSegment, NetworkSegmentType};
use sqlx::{FromRow, PgConnection};

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

/// One persisted policy, including cleared policies used for optimistic concurrency.
#[derive(Debug, FromRow)]
pub struct DhcpRange {
    /// Owning network segment.
    pub segment_id: NetworkSegmentId,
    /// The segment's single IPv4 prefix.
    pub prefix_id: NetworkPrefixId,
    /// Network CIDR, unchanged by range configuration.
    pub prefix: IpNetwork,
    /// Inclusive first dynamic address; absent for the default allocation policy.
    pub range_start: Option<IpAddr>,
    /// Inclusive final dynamic address; absent with `range_start`.
    pub range_end: Option<IpAddr>,
    /// Monotonically increasing policy revision.
    pub revision: i64,
}

/// Read policies for the requested segments; an empty selection returns no rows.
pub async fn find(
    txn: impl DbReader<'_>,
    ids: &[NetworkSegmentId],
) -> DatabaseResult<Vec<DhcpRange>> {
    let query = "SELECT np.segment_id, np.id AS prefix_id, np.prefix,
        r.range_start, r.range_end, r.revision
        FROM network_dhcp_ranges r JOIN network_prefixes np ON np.id = r.prefix_id
        JOIN network_segments ns ON ns.id = np.segment_id
        WHERE np.segment_id = ANY($1) AND ns.deleted IS NULL ORDER BY np.segment_id";
    sqlx::query_as(query)
        .bind(ids)
        .fetch_all(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Read current bounds while the caller holds its segment allocation lock.
/// Bounds are deliberately read from the database, not a possibly older segment snapshot.
pub(crate) async fn bounds(
    txn: impl DbReader<'_>,
    prefix_id: NetworkPrefixId,
) -> DatabaseResult<Option<(IpAddr, IpAddr)>> {
    let query = "SELECT range_start, range_end FROM network_dhcp_ranges
        WHERE prefix_id = $1 AND range_start IS NOT NULL";
    sqlx::query_as(query)
        .bind(prefix_id)
        .fetch_optional(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))
}

/// Set or clear an interval after taking the exclusive segment advisory lock.
/// Fixed allocations may remain outside it. A change that strands existing dynamic
/// allocations fails atomically; it never reclaims addresses or rewrites leases.
pub async fn set(
    txn: &mut PgConnection,
    segment: &NetworkSegment,
    range: Option<(IpAddr, IpAddr)>,
    if_revision: i64,
) -> DatabaseResult<DhcpRange> {
    if segment.is_marked_as_deleted()
        || segment.config.segment_type == NetworkSegmentType::Tenant
        || segment.config.name == "static-assignments"
    {
        return Err(DatabaseError::InvalidArgument(
            "DHCP pool: only active site segments are supported".into(),
        ));
    }
    let prefixes = segment
        .prefixes
        .iter()
        .filter(|p| p.prefix.is_ipv4())
        .collect::<Vec<_>>();
    let [prefix] = prefixes.as_slice() else {
        return Err(DatabaseError::InvalidArgument(
            "DHCP pool: exactly one IPv4 prefix is required".into(),
        ));
    };
    if let Some((start, end)) = range {
        let IpNetwork::V4(network) = prefix.prefix else {
            unreachable!()
        };
        let valid = matches!((start, end), (IpAddr::V4(_), IpAddr::V4(_)))
            && start <= end
            && prefix.prefix.contains(start)
            && prefix.prefix.contains(end)
            && start > IpAddr::V4(network.network())
            && end < IpAddr::V4(network.broadcast())
            && !prefix.gateway.is_some_and(|ip| start <= ip && ip <= end)
            && !prefix.svi_ip.is_some_and(|ip| start <= ip && ip <= end);
        if !valid {
            return Err(DatabaseError::InvalidArgument(
                "DHCP pool: invalid IPv4 range or includes gateway/SVI".into(),
            ));
        }
        // Segment advisory exclusion makes this check and the update atomic with DHCP writers.
        let query = "SELECT mia.address FROM machine_interface_addresses mia
            JOIN machine_interfaces mi ON mi.id = mia.interface_id
            WHERE mi.segment_id = $1 AND mia.allocation_type = 'dhcp'
              AND family(mia.address) = 4
              AND NOT (mia.address BETWEEN $2::inet AND $3::inet)
            ORDER BY mia.address LIMIT 20";
        let outside: Vec<IpAddr> = sqlx::query_scalar(query)
            .bind(segment.id)
            .bind(start)
            .bind(end)
            .fetch_all(&mut *txn)
            .await
            .map_err(|e| DatabaseError::query(query, e))?;
        if !outside.is_empty() {
            return Err(DatabaseError::FailedPrecondition(format!(
                "DHCP pool: existing dynamic addresses outside range: {}",
                outside
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            )));
        }
    }
    let current = find(&mut *txn, &[segment.id]).await?.into_iter().next();
    let revision = current.as_ref().map_or(0, |row| row.revision);
    if if_revision != revision {
        return Err(DatabaseError::ConcurrentModificationError(
            "DHCP pool",
            revision.to_string(),
        ));
    }
    let revision = revision
        .checked_add(1)
        .ok_or_else(|| DatabaseError::internal("DHCP pool revision overflow".to_string()))?;
    let query = "INSERT INTO network_dhcp_ranges (prefix_id, range_start, range_end, revision)
        VALUES ($1, $2::inet, $3::inet, $4)
        ON CONFLICT (prefix_id) DO UPDATE SET
            range_start = EXCLUDED.range_start, range_end = EXCLUDED.range_end,
            revision = EXCLUDED.revision";
    sqlx::query(query)
        .bind(prefix.id)
        .bind(range.map(|r| r.0))
        .bind(range.map(|r| r.1))
        .bind(revision)
        .execute(&mut *txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;
    Ok(DhcpRange {
        segment_id: segment.id,
        prefix_id: prefix.id,
        prefix: prefix.prefix,
        range_start: range.map(|r| r.0),
        range_end: range.map(|r| r.1),
        revision,
    })
}
