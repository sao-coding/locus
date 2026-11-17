-- Synthetic stand-in for TIPX_dev.student. Replace with the real DDL
-- (SHOW CREATE TABLE student) once the real table is reachable.
CREATE DATABASE IF NOT EXISTS bench CHARACTER SET utf8mb4;
USE bench;
DROP TABLE IF EXISTS student;
CREATE TABLE student (
  id INT UNSIGNED NOT NULL AUTO_INCREMENT,
  student_no CHAR(10) NOT NULL,
  name VARCHAR(50) NOT NULL,
  gender ENUM('M','F') NOT NULL,
  birthday DATE NOT NULL,
  email VARCHAR(100) NOT NULL,
  phone VARCHAR(20) NOT NULL,
  class_id INT NOT NULL,
  department VARCHAR(50) NOT NULL,
  enroll_year SMALLINT NOT NULL,
  gpa DECIMAL(4,2) NOT NULL,
  credits INT NOT NULL,
  address VARCHAR(255) NOT NULL,
  note TEXT NULL,
  profile JSON NOT NULL,
  created_at DATETIME(3) NOT NULL,
  updated_at TIMESTAMP NOT NULL,
  PRIMARY KEY (id),
  UNIQUE KEY uk_student_no (student_no),
  KEY idx_class (class_id),
  KEY idx_dept_year (department, enroll_year)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
