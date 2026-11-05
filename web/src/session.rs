#![cfg_attr(not(test), forbid(unsafe_code))]

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Login,
    Snapshot,
    Logout,
}

impl Operation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Login => "login",
            Self::Snapshot => "snapshot",
            Self::Logout => "logout",
        }
    }
}

pub struct Request {
    pub space: String,
    pub password: String,
    pub generation: u32,
    pub operation: Operation,
}

#[derive(Default)]
pub struct Session {
    pub space: String,
    password: Option<String>,
    generation: u32,
    pending: Option<Operation>,
    active: bool,
    pub restoring: bool,
}

impl Session {
    pub fn unlock(&mut self, space: String, password: String) {
        self.restore(space);
        self.password = Some(password);
        self.restoring = false;
    }

    pub fn restore(&mut self, space: String) {
        self.lock();
        self.space = space;
        self.active = true;
        self.restoring = true;
    }

    pub fn authenticated(&mut self) {
        self.restoring = false;
    }

    pub fn request(&mut self) -> Option<Request> {
        if self.pending.is_some() || !self.active {
            return None;
        }
        let password = self.password.take();
        let operation = if password.is_some() {
            Operation::Login
        } else {
            Operation::Snapshot
        };
        Some(self.begin(operation, password.unwrap_or_default()))
    }

    pub fn logout(&mut self) -> Option<Request> {
        if !self.active || self.pending == Some(Operation::Logout) {
            return None;
        }
        self.password = None;
        // 主动退出使正在飞行的刷新失效，不等待它完成。
        Some(self.begin(Operation::Logout, String::new()))
    }

    fn begin(&mut self, operation: Operation, password: String) -> Request {
        self.generation = self.generation.wrapping_add(1);
        self.pending = Some(operation);
        Request {
            space: self.space.clone(),
            password,
            generation: self.generation,
            operation,
        }
    }

    pub fn finish(&mut self, generation: u32) -> Option<Operation> {
        if self.generation != generation {
            return None;
        }
        self.pending.take()
    }

    pub fn lock(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.password = None;
        self.pending = None;
        self.active = false;
        self.restoring = false;
    }
}

#[cfg(test)]
mod tests {
    use super::{Operation, Session};

    #[test]
    fn password_is_used_once_then_requests_use_the_cookie() {
        let mut state = Session::default();
        state.unlock("work".into(), "虚构密码".into());
        let login = state.request().unwrap();
        assert_eq!(login.space, "work");
        assert_eq!(login.operation, Operation::Login);
        assert_eq!(login.operation.as_str(), "login");
        assert_eq!(login.password, "虚构密码");
        assert!(state.password.is_none());
        assert!(state.request().is_none());
        assert_eq!(state.finish(login.generation), Some(Operation::Login));
        state.authenticated();
        assert!(!state.restoring);
        let refresh = state.request().unwrap();
        assert_eq!(refresh.operation, Operation::Snapshot);
        assert_eq!(refresh.operation.as_str(), "snapshot");
        assert!(refresh.password.is_empty());
    }

    #[test]
    fn restore_needs_no_password_and_logout_invalidates_old_responses() {
        let mut state = Session::default();
        state.restore("work".into());
        let restore = state.request().unwrap();
        assert!(restore.password.is_empty());
        assert!(state.restoring);
        let logout = state.logout().unwrap();
        assert_eq!(state.finish(restore.generation), None);
        assert!(state.request().is_none());
        assert_eq!(state.finish(logout.generation), Some(Operation::Logout));
        state.lock();
        assert!(state.request().is_none());
        state.restore("life".into());
        let life = state.request().unwrap();
        assert_eq!(state.finish(logout.generation), None);
        assert_eq!(state.finish(life.generation), Some(Operation::Snapshot));
        assert_eq!(state.finish(life.generation), None);
    }
}
