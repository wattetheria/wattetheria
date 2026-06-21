use super::{SocialStore, bool_to_sqlite, parse_json_array, sqlite_to_bool};
use crate::domain::agent_skills::AgentSkill;
use crate::types::{SocialError, SocialResult};
use rusqlite::params;

impl SocialStore {
    pub fn list_agent_skills(&self) -> SocialResult<Vec<AgentSkill>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT skill_id, name, description, tags_json, visible, source, sort_order, created_at, updated_at
                 FROM agent_skills
                 ORDER BY sort_order ASC, name ASC, skill_id ASC",
            )
            .map_err(|error| {
                SocialError::Storage(format!("prepare agent skills query: {error}"))
            })?;
        let rows = stmt
            .query_map([], row_to_agent_skill)
            .map_err(|error| SocialError::Storage(format!("query agent skills: {error}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| SocialError::Storage(format!("collect agent skills: {error}")))
    }

    pub fn list_visible_agent_skills(&self) -> SocialResult<Vec<AgentSkill>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT skill_id, name, description, tags_json, visible, source, sort_order, created_at, updated_at
                 FROM agent_skills
                 WHERE visible = 1
                 ORDER BY sort_order ASC, name ASC, skill_id ASC",
            )
            .map_err(|error| {
                SocialError::Storage(format!("prepare visible agent skills query: {error}"))
            })?;
        let rows = stmt.query_map([], row_to_agent_skill).map_err(|error| {
            SocialError::Storage(format!("query visible agent skills: {error}"))
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| SocialError::Storage(format!("collect visible agent skills: {error}")))
    }

    pub fn upsert_agent_skill(&self, skill: &AgentSkill) -> SocialResult<()> {
        let tags = serde_json::to_string(&skill.tags).map_err(|error| {
            SocialError::Storage(format!("serialize agent skill tags: {error}"))
        })?;
        self.conn()?
            .execute(
                "INSERT INTO agent_skills (
                    skill_id, name, description, tags_json, visible, source, sort_order, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(skill_id) DO UPDATE SET
                    name = excluded.name,
                    description = excluded.description,
                    tags_json = excluded.tags_json,
                    visible = excluded.visible,
                    source = excluded.source,
                    sort_order = excluded.sort_order,
                    updated_at = excluded.updated_at",
                params![
                    skill.skill_id,
                    skill.name,
                    skill.description,
                    tags,
                    bool_to_sqlite(skill.visible),
                    skill.source,
                    skill.sort_order,
                    skill.created_at,
                    skill.updated_at,
                ],
            )
            .map_err(|error| SocialError::Storage(format!("upsert agent skill: {error}")))?;
        Ok(())
    }

    pub fn delete_agent_skill(&self, skill_id: &str) -> SocialResult<()> {
        self.conn()?
            .execute(
                "DELETE FROM agent_skills WHERE skill_id = ?1",
                params![skill_id],
            )
            .map_err(|error| SocialError::Storage(format!("delete agent skill: {error}")))?;
        Ok(())
    }
}

fn row_to_agent_skill(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentSkill> {
    Ok(AgentSkill {
        skill_id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        tags: parse_json_array(row.get::<_, String>(3)?)?,
        visible: sqlite_to_bool(row.get::<_, i64>(4)?),
        source: row.get(5)?,
        sort_order: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}
