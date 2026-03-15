use crate::identity::schema::IdentityPermissions;

pub fn is_skill_allowed(permissions: &IdentityPermissions, skill_name: &str) -> bool {
    if permissions
        .denied_skills
        .iter()
        .any(|name| name.eq_ignore_ascii_case(skill_name))
    {
        return false;
    }

    if permissions.allowed_skills.is_empty() {
        return false;
    }

    permissions
        .allowed_skills
        .iter()
        .any(|name| name.eq_ignore_ascii_case(skill_name) || name == "*")
}

#[cfg(test)]
mod tests {
    use crate::identity::schema::IdentityPermissions;

    use super::is_skill_allowed;

    #[test]
    fn deny_overrides_allow() {
        let p = IdentityPermissions {
            allowed_skills: vec!["*".to_string()],
            denied_skills: vec!["memory.write".to_string()],
        };

        assert!(!is_skill_allowed(&p, "memory.write"));
        assert!(is_skill_allowed(&p, "agent.status"));
    }
}
