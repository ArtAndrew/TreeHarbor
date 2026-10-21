use crate::{
    ChangeOperation, Changes, Commit, Directory, Entry, EntryMode, EntryRef, Error, FRAME_HEADER,
    MAX_ENTRIES, MAX_PARENTS, MAX_PAYLOAD, Name, Object, ObjectId, RepoPath, Result,
};

const MAGIC: &[u8; 11] = b"treeharbor\0";
const VERSION: u16 = 1;

impl Object {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut w = Writer(Vec::new());
        match self {
            Self::Blob(bytes) => w.raw(bytes)?,
            Self::Directory(dir) => {
                w.count(dir.entries().len())?;
                for e in dir.entries() {
                    w.bytes(e.name.as_bytes())?;
                    w.entry(e.value)?;
                }
            }
            Self::Snapshot { root } => w.id(*root)?,
            Self::ChangeOperations(changes) => {
                w.count(changes.operations().len())?;
                for op in changes.operations() {
                    w.bytes(op.path.as_bytes())?;
                    w.optional(op.expected)?;
                    w.optional(op.replacement)?;
                }
            }
            Self::NativeCommit(c) => {
                c.validate()?;
                w.count(c.parents.len())?;
                for parent in &c.parents {
                    w.id(*parent)?;
                }
                w.id(c.snapshot)?;
                w.id(c.changes)?;
                w.bytes(c.author_name.as_bytes())?;
                w.bytes(c.author_email.as_bytes())?;
                w.raw(&c.timestamp_seconds.to_be_bytes())?;
                w.raw(&c.timezone_minutes.to_be_bytes())?;
                w.bytes(c.message.as_bytes())?;
            }
        }
        let mut out = Vec::with_capacity(FRAME_HEADER + w.0.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_be_bytes());
        out.push(self.kind());
        out.extend_from_slice(&(w.0.len() as u64).to_be_bytes());
        out.extend(w.0);
        Ok(out)
    }

    pub fn decode(frame: &[u8]) -> Result<Self> {
        if frame.len() > FRAME_HEADER + MAX_PAYLOAD {
            return Err(Error::TooLarge);
        }
        let mut r = Reader {
            bytes: frame,
            at: 0,
        };
        if r.take(MAGIC.len())? != MAGIC {
            return Err(Error::InvalidObject("magic"));
        }
        let version = u16::from_be_bytes(r.array()?);
        if version != VERSION {
            return Err(Error::UnsupportedVersion(version));
        }
        let kind = r.byte()?;
        let length = u64::from_be_bytes(r.array()?);
        if length > MAX_PAYLOAD as u64 {
            return Err(Error::TooLarge);
        }
        if length != r.remaining() as u64 {
            return Err(Error::InvalidObject("payload length"));
        }
        let object = match kind {
            1 => Self::Blob(r.take(length as usize)?.to_vec()),
            2 => {
                let count = r.count(MAX_ENTRIES)?;
                let mut entries: Vec<Entry> = Vec::with_capacity(count);
                for _ in 0..count {
                    let name = Name::new(r.bytes()?.to_vec())?;
                    if entries.last().is_some_and(|e| e.name >= name) {
                        return Err(Error::InvalidObject("directory order"));
                    }
                    entries.push(Entry {
                        name,
                        value: r.entry()?,
                    });
                }
                Self::Directory(Directory::new(entries)?)
            }
            3 => Self::Snapshot { root: r.id()? },
            4 => {
                let count = r.count(MAX_ENTRIES)?;
                let mut ops: Vec<ChangeOperation> = Vec::with_capacity(count);
                for _ in 0..count {
                    let path = RepoPath::new(r.bytes()?.to_vec())?;
                    if ops.last().is_some_and(|op| op.path >= path) {
                        return Err(Error::InvalidObject("change order"));
                    }
                    ops.push(ChangeOperation {
                        path,
                        expected: r.optional()?,
                        replacement: r.optional()?,
                    });
                }
                Self::ChangeOperations(Changes::new(ops)?)
            }
            5 => {
                let count = r.count(MAX_PARENTS)?;
                let mut parents = Vec::with_capacity(count);
                for _ in 0..count {
                    parents.push(r.id()?);
                }
                let c = Commit {
                    parents,
                    snapshot: r.id()?,
                    changes: r.id()?,
                    author_name: r.text()?,
                    author_email: r.text()?,
                    timestamp_seconds: i64::from_be_bytes(r.array()?),
                    timezone_minutes: i16::from_be_bytes(r.array()?),
                    message: r.text()?,
                };
                c.validate()?;
                Self::NativeCommit(c)
            }
            _ => return Err(Error::InvalidObject("unknown kind")),
        };
        if r.remaining() != 0 {
            return Err(Error::InvalidObject("trailing payload"));
        }
        Ok(object)
    }
    pub fn decode_verified(id: ObjectId, frame: &[u8]) -> Result<Self> {
        if frame.len() > FRAME_HEADER + MAX_PAYLOAD {
            return Err(Error::TooLarge);
        }
        if ObjectId::hash_frame(frame) != id {
            return Err(Error::InvalidObject("digest mismatch"));
        }
        Self::decode(frame)
    }
}

struct Writer(Vec<u8>);
impl Writer {
    fn raw(&mut self, b: &[u8]) -> Result<()> {
        if b.len() > MAX_PAYLOAD - self.0.len() {
            return Err(Error::TooLarge);
        }
        self.0.extend_from_slice(b);
        Ok(())
    }
    fn count(&mut self, count: usize) -> Result<()> {
        self.raw(
            &u32::try_from(count)
                .map_err(|_| Error::TooLarge)?
                .to_be_bytes(),
        )
    }
    fn bytes(&mut self, b: &[u8]) -> Result<()> {
        self.count(b.len())?;
        self.raw(b)
    }
    fn id(&mut self, id: ObjectId) -> Result<()> {
        self.raw(id.as_bytes())
    }
    fn entry(&mut self, e: EntryRef) -> Result<()> {
        self.raw(&(e.mode as u32).to_be_bytes())?;
        self.id(e.target)
    }
    fn optional(&mut self, e: Option<EntryRef>) -> Result<()> {
        match e {
            None => self.raw(&[0]),
            Some(e) => {
                self.raw(&[1])?;
                self.entry(e)
            }
        }
    }
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn remaining(&self) -> usize {
        self.bytes.len() - self.at
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(n).ok_or(Error::TooLarge)?;
        let bytes = self
            .bytes
            .get(self.at..end)
            .ok_or(Error::InvalidObject("truncated payload"))?;
        self.at = end;
        Ok(bytes)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.take(N)?
            .try_into()
            .map_err(|_| Error::InvalidObject("fixed field"))
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }
    fn count(&mut self, max: usize) -> Result<usize> {
        let n = u32::from_be_bytes(self.array()?) as usize;
        if n > max {
            return Err(Error::TooLarge);
        }
        Ok(n)
    }
    fn bytes(&mut self) -> Result<&'a [u8]> {
        let n = self.count(MAX_PAYLOAD)?;
        self.take(n)
    }
    fn text(&mut self) -> Result<String> {
        String::from_utf8(self.bytes()?.to_vec()).map_err(|_| Error::InvalidObject("non-UTF8 text"))
    }
    fn id(&mut self) -> Result<ObjectId> {
        Ok(ObjectId::from_bytes(self.array()?))
    }
    fn entry(&mut self) -> Result<EntryRef> {
        Ok(EntryRef {
            mode: EntryMode::try_from(u32::from_be_bytes(self.array()?))?,
            target: self.id()?,
        })
    }
    fn optional(&mut self) -> Result<Option<EntryRef>> {
        match self.byte()? {
            0 => Ok(None),
            1 => Ok(Some(self.entry()?)),
            _ => Err(Error::InvalidObject("optional tag")),
        }
    }
}
