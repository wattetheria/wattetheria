use std::collections::VecDeque;

use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};

use super::SpacePackage;

const DIRECTIONS: [u8; 4] = *b"URDL";

impl SpacePackage {
    pub fn destination_tile(&self, agent_did: &str, place_id: &str) -> Result<[u32; 2]> {
        let place = self.place(place_id).context("unknown destination place")?;
        let reachable = self.spawn_reachable()?;
        let mut candidates: Vec<_> = place
            .tiles
            .iter()
            .copied()
            .filter(|tile| reachable[self.index(*tile)])
            .collect();
        candidates.sort_by_key(|tile| (tile[1], tile[0]));
        ensure!(
            !candidates.is_empty(),
            "place has no tiles reachable from spawn"
        );
        let hash = Sha256::digest(format!("{agent_did}\n{place_id}"));
        let selector = u64::from_be_bytes(hash[..8].try_into().expect("SHA256 has eight bytes"));
        let index = usize::try_from(selector % candidates.len() as u64)?;
        Ok(candidates[index])
    }

    pub fn path(&self, from: [u32; 2], to: [u32; 2]) -> Result<String> {
        ensure!(
            self.is_walkable(from) && self.is_walkable(to),
            "route endpoint is blocked or out of bounds"
        );
        if from == to {
            return Ok(String::new());
        }
        let mut visited = vec![false; self.cell_count()];
        let mut previous = vec![None; self.cell_count()];
        let mut queue = VecDeque::from([from]);
        visited[self.index(from)] = true;
        while let Some(tile) = queue.pop_front() {
            for direction in DIRECTIONS {
                let Some(next) = self.step(tile, direction) else {
                    continue;
                };
                let index = self.index(next);
                if visited[index] {
                    continue;
                }
                visited[index] = true;
                previous[index] = Some((tile, direction));
                if next == to {
                    let mut path = Vec::new();
                    let mut cursor = to;
                    while cursor != from {
                        let (parent, direction) =
                            previous[self.index(cursor)].context("route predecessor missing")?;
                        path.push(direction);
                        cursor = parent;
                    }
                    path.reverse();
                    return Ok(String::from_utf8(path)?);
                }
                queue.push_back(next);
            }
        }
        bail!("destination is unreachable from current tile")
    }

    pub(super) fn spawn_reachable(&self) -> Result<Vec<bool>> {
        let spawn = self
            .place(&self.spawn_place_id)
            .context("unknown spawn place")?;
        let start = spawn
            .tiles
            .iter()
            .copied()
            .filter(|tile| self.is_walkable(*tile))
            .min_by_key(|tile| (tile[1], tile[0]))
            .context("spawn place has no walkable tile")?;
        let mut reachable = vec![false; self.cell_count()];
        reachable[self.index(start)] = true;
        let mut queue = VecDeque::from([start]);
        while let Some(tile) = queue.pop_front() {
            for direction in DIRECTIONS {
                let Some(next) = self.step(tile, direction) else {
                    continue;
                };
                let index = self.index(next);
                if !reachable[index] {
                    reachable[index] = true;
                    queue.push_back(next);
                }
            }
        }
        Ok(reachable)
    }

    pub(super) fn step(&self, tile: [u32; 2], direction: u8) -> Option<[u32; 2]> {
        let next = match direction {
            b'U' => [tile[0], tile[1].checked_sub(1)?],
            b'R' => [tile[0].checked_add(1)?, tile[1]],
            b'D' => [tile[0], tile[1].checked_add(1)?],
            b'L' => [tile[0].checked_sub(1)?, tile[1]],
            _ => return None,
        };
        self.is_walkable(next).then_some(next)
    }

    pub(super) fn index(&self, tile: [u32; 2]) -> usize {
        tile[1] as usize * self.size[0] as usize + tile[0] as usize
    }

    fn cell_count(&self) -> usize {
        self.size[0] as usize * self.size[1] as usize
    }
}
