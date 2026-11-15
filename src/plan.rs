//! Print modes: how an image is turned into one or more engraving jobs, and how long
//! they take.
//!
//! The V10 firmware only engraves row by row, in a zig-zag: on each row the head goes
//! from the first to the last black pixel, burning the black ones and crossing the white
//! ones in between; empty rows and the white margins are skipped. There is no "laser on"
//! or "go to X, Y" command, so a true vector path is impossible. What the software can do
//! is choose *what* to send:
//!
//! - [`PrintMode::Single`]: the image as it is, one job;
//! - [`PrintMode::Split`]: the image cut in vertical bands sent one after the other, when
//!   the white pixels no longer crossed are worth more than the extra uploads. The bands
//!   keep the coordinates of the whole image, so they line up on the piece as long as the
//!   engraver is not restarted in between;
//! - [`PrintMode::Line`]: shapes reduced to their centre line, drawn with a stroke of a
//!   few pixels instead of being filled, then split like the previous mode.
//!
//! The timing model and the band search come from measurements on a real DK-8-KZ
//! (14–15/11/2026) and follow the reference tool `ottimizza_lavori.py` operation by
//! operation, so both choose the same cuts.

use std::ops::Range;

use image::{GrayImage, Luma, Rgb, RgbImage};

use crate::protocol::{Protocol, Version};
use crate::stroke;

/// Milliseconds for a burnt pixel: this plus [`BURN_MS_PER_UNIT`] × burn time
/// (measured at burn time 10, 20 and 40).
pub const BURN_MS_FIXED: f64 = 2.5;
pub const BURN_MS_PER_UNIT: f64 = 0.5;
/// Milliseconds for a white pixel crossed between two black stretches of the same row.
pub const WHITE_MS: f64 = 1.73;
/// Milliseconds for moving to the next row.
pub const ROW_MS: f64 = 1.0;
/// Seconds every job costs besides the engraving: 3.5 s of erase, 5.7 s of data and about
/// 3 s between the start command, the run to the first pixel and the end message.
/// (Summed in this order to get the same floating-point value as the reference tool.)
pub const JOB_OVERHEAD_S: f64 = 3.5 + 5.7 + 3.0;

/// Widest stroke offered by the line mode, in pixels.
pub const MAX_STROKE: u8 = 6;

/// How the image is sent to the engraver.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PrintMode {
    /// One upload, one start: what the original program does.
    #[default]
    Single,
    /// Vertical bands engraved one after the other to avoid crossing empty areas.
    Split,
    /// Centre lines `stroke` pixels wide instead of filled shapes, split when it helps.
    Line { stroke: u8 },
}

/// What a job makes the head do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cost {
    /// Pixels burnt.
    pub burnt: u64,
    /// White pixels crossed between the first and the last black pixel of each row.
    pub crossed: u64,
    /// Rows with at least one black pixel.
    pub rows: u64,
}

impl Cost {
    /// Estimated duration in seconds, upload included; zero for an empty job.
    pub fn seconds(&self, burn: u8) -> f64 {
        if self.rows == 0 {
            return 0.0;
        }
        let burning = self.burnt as f64 * (BURN_MS_FIXED + BURN_MS_PER_UNIT * f64::from(burn));
        (burning + self.crossed as f64 * WHITE_MS + self.rows as f64 * ROW_MS) / 1000.0 + JOB_OVERHEAD_S
    }
}

/// True for the pixels the engraver burns: everything that is not pure white.
fn burns(image: &GrayImage, x: u32, y: u32) -> bool {
    image.get_pixel(x, y).0[0] != 255
}

/// First and last burnt column of each row within `columns`, and the number of burnt pixels.
#[derive(Clone, Copy, Default)]
struct RowSpan {
    first: u32,
    last: u32,
    burnt: u32,
}

/// Running cost of the band `[start, end)` while `end` grows one column at a time.
struct Band {
    rows: Vec<RowSpan>,
    cost: Cost,
}

impl Band {
    fn new(height: u32) -> Self {
        Band { rows: vec![RowSpan::default(); height as usize], cost: Cost::default() }
    }

    /// Adds column `x`, which must be to the right of every column added so far;
    /// `burnt_rows` are the rows of its burnt pixels.
    fn push_column(&mut self, x: u32, burnt_rows: &[u32]) {
        for &y in burnt_rows {
            let row = &mut self.rows[y as usize];
            if row.burnt == 0 {
                row.first = x;
                self.cost.rows += 1;
            } else {
                // The white pixels between the previous last black pixel and this one.
                self.cost.crossed += u64::from(x - row.last - 1);
            }
            row.last = x;
            row.burnt += 1;
            self.cost.burnt += 1;
        }
    }
}

/// The rows of the burnt pixels of each column.
fn burnt_rows(image: &GrayImage) -> Vec<Vec<u32>> {
    (0..image.width()).map(|x| (0..image.height()).filter(|&y| burns(image, x, y)).collect()).collect()
}

/// Cost of engraving the part of `image` inside `columns` as one job.
pub fn cost(image: &GrayImage, columns: Range<u32>) -> Cost {
    let mut band = Band::new(image.height());
    for x in columns.start..columns.end.min(image.width()) {
        let rows: Vec<u32> = (0..image.height()).filter(|&y| burns(image, x, y)).collect();
        band.push_column(x, &rows);
    }
    band.cost
}

/// The vertical bands that minimise the total estimated time at burn time `burn`.
///
/// Dynamic programming over the columns: `best[b]` is the minimum time to engrave the
/// columns before `b`, trying every possible last band `[a, b)`. Ties go to the leftmost
/// cut, as in the reference tool. Empty bands cost nothing and are dropped.
pub fn best_bands(image: &GrayImage, burn: u8) -> Vec<Range<u32>> {
    let columns = burnt_rows(image);
    let used: Vec<u32> = (0..image.width()).filter(|&x| !columns[x as usize].is_empty()).collect();
    let (Some(&start), Some(&last)) = (used.first(), used.last()) else {
        return Vec::new();
    };
    let end = last + 1;
    let n = (end - start) as usize;
    // band_seconds[a][b] for start <= a < b <= end, stored by offset from `start`.
    let mut best = vec![f64::INFINITY; n + 1];
    let mut from = vec![0usize; n + 1];
    best[0] = 0.0;
    // Bands are grown column by column from each start; the candidates of every end
    // column are compared in order of start, so the first minimum wins.
    let mut seconds = vec![vec![0.0f64; n + 1]; n];
    for (a, row) in seconds.iter_mut().enumerate() {
        let mut band = Band::new(image.height());
        for b in a + 1..=n {
            let x = start + b as u32 - 1;
            band.push_column(x, &columns[x as usize]);
            row[b] = band.cost.seconds(burn);
        }
    }
    for b in 1..=n {
        for a in 0..b {
            let total = best[a] + seconds[a][b];
            if total < best[b] {
                best[b] = total;
                from[b] = a;
            }
        }
    }
    let mut bands = Vec::new();
    let mut b = n;
    while b != 0 {
        let a = from[b];
        bands.push(start + a as u32..start + b as u32);
        b = a;
    }
    bands.reverse();
    bands.retain(|band| cost(image, band.clone()).rows != 0);
    bands
}

/// One upload followed by one start.
#[derive(Clone, Debug)]
pub struct Job {
    /// Columns of the whole image this job engraves.
    pub columns: Range<u32>,
    /// The image to send: the whole area, white outside `columns`.
    pub image: GrayImage,
    pub cost: Cost,
    /// Estimated duration, upload included.
    pub seconds: f64,
}

/// An image prepared for a print mode.
#[derive(Clone, Debug)]
pub struct Plan {
    pub mode: PrintMode,
    /// Everything that will be engraved, all jobs together.
    pub image: GrayImage,
    pub jobs: Vec<Job>,
    /// Estimated total duration.
    pub seconds: f64,
    /// Estimated duration of the original image as a single job, for comparison.
    pub single_seconds: f64,
}

impl Plan {
    /// Prepares `base` (the image shown in the preview, black = burn) for `mode` at burn
    /// time `burn`. Splitting relies on behaviour measured on the V10 firmware only: with
    /// the other protocols the image always stays in one job.
    pub fn new(base: &GrayImage, mode: PrintMode, burn: u8, protocol: Protocol) -> Plan {
        let image = match mode {
            PrintMode::Line { stroke } => stroke::line_art(base, stroke.clamp(1, MAX_STROKE)),
            PrintMode::Single | PrintMode::Split => base.clone(),
        };
        let whole = 0..image.width();
        let can_split = protocol.version == Version::V10 && mode != PrintMode::Single;
        let mut bands = if can_split { best_bands(&image, burn) } else { Vec::new() };
        if bands.is_empty() {
            bands.push(whole.clone());
        }
        let jobs: Vec<Job> = bands
            .into_iter()
            .map(|columns| {
                let cost = cost(&image, columns.clone());
                Job { image: band_image(&image, &columns), seconds: cost.seconds(burn), cost, columns }
            })
            .collect();
        Plan {
            mode,
            seconds: jobs.iter().map(|job| job.seconds).sum(),
            single_seconds: cost(base, 0..base.width()).seconds(burn),
            image,
            jobs,
        }
    }

    /// Total cost of all the jobs.
    pub fn cost(&self) -> Cost {
        self.jobs.iter().fold(Cost::default(), |sum, job| Cost {
            burnt: sum.burnt + job.cost.burnt,
            crossed: sum.crossed + job.cost.crossed,
            rows: sum.rows + job.cost.rows,
        })
    }

    /// True when there is nothing to burn.
    pub fn is_empty(&self) -> bool {
        self.cost().burnt == 0
    }

    /// Seconds saved compared with the original image as a single job (negative when
    /// this plan is slower).
    pub fn saved_seconds(&self) -> f64 {
        self.single_seconds - self.seconds
    }

    /// Picture of the plan: what is burnt in black, the white pixels the head crosses in
    /// [`TRAVEL`], those it no longer crosses thanks to the split in [`SAVED`], and a
    /// [`CUT`] line between two jobs.
    pub fn preview(&self) -> RgbImage {
        let (w, h) = self.image.dimensions();
        let mut out = RgbImage::from_pixel(w, h, Rgb([255, 255, 255]));
        let whole = spans(&self.image, &(0..w));
        let job_spans: Vec<Vec<Option<(u32, u32)>>> =
            self.jobs.iter().map(|job| spans(&self.image, &job.columns)).collect();
        for y in 0..h {
            let crossed_alone = |x: u32| whole[y as usize].is_some_and(|(first, last)| (first..=last).contains(&x));
            let crossed_now = |x: u32| {
                job_spans.iter().any(|s| s[y as usize].is_some_and(|(first, last)| (first..=last).contains(&x)))
            };
            for x in 0..w {
                let colour = if burns(&self.image, x, y) {
                    Rgb([0, 0, 0])
                } else if crossed_now(x) {
                    TRAVEL
                } else if crossed_alone(x) {
                    SAVED
                } else {
                    continue;
                };
                out.put_pixel(x, y, colour);
            }
        }
        for job in self.jobs.iter().skip(1) {
            // Dashed, and never over a burnt pixel: the drawing stays readable.
            for y in (0..h).filter(|y| y % 6 < 3) {
                if job.columns.start < w && !burns(&self.image, job.columns.start, y) {
                    out.put_pixel(job.columns.start, y, CUT);
                }
            }
        }
        out
    }
}

/// Colours of [`Plan::preview`].
pub const TRAVEL: Rgb<u8> = Rgb([255, 214, 153]);
pub const SAVED: Rgb<u8> = Rgb([190, 235, 200]);
pub const CUT: Rgb<u8> = Rgb([10, 132, 255]);

/// `image` with everything outside `columns` turned white.
fn band_image(image: &GrayImage, columns: &Range<u32>) -> GrayImage {
    GrayImage::from_fn(image.width(), image.height(), |x, y| {
        if columns.contains(&x) { *image.get_pixel(x, y) } else { Luma([255]) }
    })
}

/// First and last burnt column of each row within `columns`.
fn spans(image: &GrayImage, columns: &Range<u32>) -> Vec<Option<(u32, u32)>> {
    (0..image.height())
        .map(|y| {
            let mut burnt = (columns.start..columns.end.min(image.width())).filter(|&x| burns(image, x, y));
            let first = burnt.next()?;
            Some((first, burnt.last().unwrap_or(first)))
        })
        .collect()
}

/// `seconds` as minutes and seconds, e.g. `3:19`.
pub fn clock(seconds: f64) -> String {
    let total = seconds.round().max(0.0) as u64;
    format!("{}:{:02}", total / 60, total % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    const V10: Protocol = Protocol { version: Version::V10, hw: None };

    fn rect(im: &mut GrayImage, x: Range<u32>, y: Range<u32>) {
        for yy in y {
            for xx in x.clone() {
                im.put_pixel(xx, yy, Luma([0]));
            }
        }
    }

    /// A thin vertical line far to the left of a block: every row pays the trip between them.
    fn far_apart() -> GrayImage {
        let mut im = GrayImage::from_pixel(490, 490, Luma([255]));
        rect(&mut im, 60..63, 100..400);
        rect(&mut im, 330..400, 150..300);
        im
    }

    #[test]
    fn cost_counts_burnt_crossed_and_rows() {
        let mut im = GrayImage::from_pixel(20, 4, Luma([255]));
        rect(&mut im, 2..4, 0..2);
        rect(&mut im, 10..11, 1..3);
        // Row 0: 2 burnt; row 1: 3 burnt and 6 crossed; row 2: 1 burnt.
        assert_eq!(cost(&im, 0..20), Cost { burnt: 6, crossed: 6, rows: 3 });
        assert_eq!(cost(&im, 0..8), Cost { burnt: 4, crossed: 0, rows: 2 });
        assert_eq!(cost(&im, 5..9), Cost::default());
    }

    #[test]
    fn seconds_follow_the_measured_model() {
        let c = Cost { burnt: 1000, crossed: 2000, rows: 100 };
        // 1000 × 22.5 ms + 2000 × 1.73 ms + 100 × 1 ms + 12.2 s
        assert!((c.seconds(40) - (22.5 + 3.46 + 0.1 + 12.2)).abs() < 1e-9);
        assert_eq!(Cost::default().seconds(40), 0.0);
    }

    #[test]
    fn distant_parts_become_two_jobs() {
        let im = far_apart();
        let plan = Plan::new(&im, PrintMode::Split, 40, V10);
        assert_eq!(plan.jobs.len(), 2);
        assert_eq!(plan.jobs[0].columns, 60..63);
        assert_eq!(plan.jobs[1].columns.end, 400);
        assert_eq!(plan.cost().crossed, 0);
        assert!(plan.saved_seconds() > 30.0, "saved only {} s", plan.saved_seconds());
        // The jobs together are the whole image, each in the coordinates of the whole.
        let mut together = GrayImage::from_pixel(490, 490, Luma([255]));
        for job in &plan.jobs {
            for (x, y, p) in job.image.enumerate_pixels() {
                if p.0[0] == 0 {
                    assert!(job.columns.contains(&x));
                    together.put_pixel(x, y, *p);
                }
            }
        }
        assert_eq!(together, im);
    }

    #[test]
    fn a_compact_shape_stays_in_one_job() {
        let mut im = GrayImage::from_pixel(490, 490, Luma([255]));
        rect(&mut im, 200..260, 200..260);
        let plan = Plan::new(&im, PrintMode::Split, 40, V10);
        assert_eq!(plan.jobs.len(), 1);
        assert_eq!(plan.seconds, plan.single_seconds);
    }

    #[test]
    fn single_mode_and_other_protocols_never_split() {
        let im = far_apart();
        assert_eq!(Plan::new(&im, PrintMode::Single, 40, V10).jobs.len(), 1);
        let v30 = Protocol::new(Version::V30, Some(11));
        assert_eq!(Plan::new(&im, PrintMode::Split, 40, v30).jobs.len(), 1);
    }

    #[test]
    fn line_mode_burns_less_than_filling() {
        let mut im = GrayImage::from_pixel(490, 490, Luma([255]));
        rect(&mut im, 100..110, 100..260); // a bar 10 px wide
        let filled = Plan::new(&im, PrintMode::Single, 40, V10);
        let thin = Plan::new(&im, PrintMode::Line { stroke: 1 }, 40, V10);
        let thick = Plan::new(&im, PrintMode::Line { stroke: 3 }, 40, V10);
        assert!(thin.cost().burnt < thick.cost().burnt);
        assert!(thick.cost().burnt < filled.cost().burnt / 2);
        assert!(thin.seconds < filled.seconds);
        assert_eq!(thin.single_seconds, filled.seconds);
    }

    #[test]
    fn preview_marks_travel_savings_and_cuts() {
        let plan = Plan::new(&far_apart(), PrintMode::Split, 40, V10);
        let picture = plan.preview();
        assert_eq!(*picture.get_pixel(61, 200), Rgb([0, 0, 0]));
        assert_eq!(*picture.get_pixel(200, 200), SAVED, "the gap is no longer crossed");
        assert_eq!(*picture.get_pixel(200, 50), Rgb([255, 255, 255]));
        assert!((0..490).any(|y| *picture.get_pixel(plan.jobs[1].columns.start, y) == CUT));
        let single = Plan::new(&far_apart(), PrintMode::Single, 40, V10).preview();
        assert_eq!(*single.get_pixel(200, 200), TRAVEL);
    }

    #[test]
    fn empty_image_has_one_empty_job() {
        let plan = Plan::new(&GrayImage::from_pixel(490, 490, Luma([255])), PrintMode::Split, 40, V10);
        assert!(plan.is_empty());
        assert_eq!(plan.seconds, 0.0);
    }

    #[test]
    fn clock_format() {
        assert_eq!(clock(199.4), "3:19");
        assert_eq!(clock(5.0), "0:05");
    }
}
