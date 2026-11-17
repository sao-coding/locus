-- Foreign-key fixture for the relations diagram (MariaDB / MySQL, InnoDB).
-- Idempotent: drops and rebuilds the whole `erfk` schema. Touches nothing else.
-- Load with scripts\fixtures\load-er-fk.ps1.
--
-- What it covers:
--   single-column FKs        address, product, orders, order_item, enrollment
--   composite FK             shipment (order_id, line_no) -> order_item (order_id, line_no)
--   self reference           employee.manager_id -> employee.id
--   referenced by several    customer <- address, orders ; orders <- order_item ; employee <- orders, employee
--   many-to-many link table  enrollment (student <-> course)
--   no relations             app_setting
--   deep chain (4 tables)    shipment -> order_item -> orders -> customer
--                            shipment -> order_item -> product -> category
--   second component         student / course / enrollment is not connected to the shop
SET NAMES utf8mb4;
SET FOREIGN_KEY_CHECKS = 0;
DROP DATABASE IF EXISTS erfk;
CREATE DATABASE erfk CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;
USE erfk;
SET FOREIGN_KEY_CHECKS = 1;

CREATE TABLE customer (
  id         INT NOT NULL AUTO_INCREMENT COMMENT '客戶編號',
  name       VARCHAR(80)  NOT NULL COMMENT '客戶名稱',
  email      VARCHAR(120) NULL COMMENT '電子郵件',
  created_at DATETIME     NOT NULL DEFAULT CURRENT_TIMESTAMP COMMENT '建立時間',
  PRIMARY KEY (id),
  UNIQUE KEY uq_customer_email (email)
) ENGINE=InnoDB COMMENT='客戶';

CREATE TABLE address (
  id          INT NOT NULL AUTO_INCREMENT COMMENT '地址編號',
  customer_id INT NOT NULL COMMENT '所屬客戶',
  city        VARCHAR(40)  NOT NULL COMMENT '城市',
  street      VARCHAR(120) NOT NULL COMMENT '街道地址',
  is_default  TINYINT(1)   NOT NULL DEFAULT 0 COMMENT '是否為預設地址',
  PRIMARY KEY (id),
  KEY idx_address_customer (customer_id),
  CONSTRAINT fk_address_customer FOREIGN KEY (customer_id) REFERENCES customer (id) ON DELETE CASCADE
) ENGINE=InnoDB COMMENT='客戶地址';

CREATE TABLE employee (
  id         INT NOT NULL AUTO_INCREMENT COMMENT '員工編號',
  name       VARCHAR(80) NOT NULL COMMENT '員工姓名',
  title      VARCHAR(60) NULL COMMENT '職稱',
  manager_id INT NULL COMMENT '直屬主管（自我參照）',
  PRIMARY KEY (id),
  KEY idx_employee_manager (manager_id),
  CONSTRAINT fk_employee_manager FOREIGN KEY (manager_id) REFERENCES employee (id) ON DELETE SET NULL
) ENGINE=InnoDB COMMENT='員工';

CREATE TABLE category (
  id   INT NOT NULL AUTO_INCREMENT COMMENT '分類編號',
  name VARCHAR(60) NOT NULL COMMENT '分類名稱',
  PRIMARY KEY (id)
) ENGINE=InnoDB COMMENT='商品分類';

CREATE TABLE product (
  id          INT NOT NULL AUTO_INCREMENT COMMENT '商品編號',
  category_id INT NOT NULL COMMENT '所屬分類',
  sku         VARCHAR(30)   NOT NULL COMMENT '貨號',
  name        VARCHAR(120)  NOT NULL COMMENT '商品名稱',
  price       DECIMAL(10,2) NOT NULL COMMENT '單價',
  PRIMARY KEY (id),
  UNIQUE KEY uq_product_sku (sku),
  KEY idx_product_category (category_id),
  CONSTRAINT fk_product_category FOREIGN KEY (category_id) REFERENCES category (id),
  CONSTRAINT ck_product_price CHECK (price >= 0)
) ENGINE=InnoDB COMMENT='商品';

CREATE TABLE orders (
  id           INT NOT NULL AUTO_INCREMENT COMMENT '訂單編號',
  customer_id  INT NOT NULL COMMENT '下單客戶',
  sales_rep_id INT NULL COMMENT '負責業務',
  status       VARCHAR(20) NOT NULL DEFAULT 'new' COMMENT '狀態：new / paid / shipped',
  ordered_at   DATETIME    NOT NULL DEFAULT CURRENT_TIMESTAMP COMMENT '下單時間',
  PRIMARY KEY (id),
  KEY idx_orders_customer (customer_id),
  KEY idx_orders_rep (sales_rep_id),
  CONSTRAINT fk_orders_customer FOREIGN KEY (customer_id)  REFERENCES customer (id),
  CONSTRAINT fk_orders_rep      FOREIGN KEY (sales_rep_id) REFERENCES employee (id) ON DELETE SET NULL
) ENGINE=InnoDB COMMENT='訂單';

-- A trigger for the Structure tab (keeps status lower case; the sample rows already are).
CREATE TRIGGER trg_orders_bu BEFORE UPDATE ON orders FOR EACH ROW SET NEW.status = LOWER(NEW.status);

CREATE TABLE order_item (
  order_id   INT NOT NULL COMMENT '訂單編號',
  line_no    INT NOT NULL COMMENT '明細行號',
  product_id INT NOT NULL COMMENT '商品',
  qty        INT NOT NULL DEFAULT 1 COMMENT '數量',
  PRIMARY KEY (order_id, line_no),
  KEY idx_item_product (product_id),
  CONSTRAINT fk_item_order   FOREIGN KEY (order_id)   REFERENCES orders (id) ON DELETE CASCADE,
  CONSTRAINT fk_item_product FOREIGN KEY (product_id) REFERENCES product (id)
) ENGINE=InnoDB COMMENT='訂單明細';

CREATE TABLE shipment (
  id         INT NOT NULL AUTO_INCREMENT COMMENT '出貨單編號',
  order_id   INT NOT NULL COMMENT '訂單編號（複合外鍵，第 1 欄）',
  line_no    INT NOT NULL COMMENT '明細行號（複合外鍵，第 2 欄）',
  tracking   VARCHAR(40) NULL COMMENT '物流單號',
  shipped_at DATETIME    NULL COMMENT '出貨時間',
  PRIMARY KEY (id),
  KEY idx_shipment_item (order_id, line_no),
  CONSTRAINT fk_shipment_item FOREIGN KEY (order_id, line_no) REFERENCES order_item (order_id, line_no)
) ENGINE=InnoDB COMMENT='出貨（複合外鍵）';

CREATE TABLE student (
  id   INT NOT NULL AUTO_INCREMENT COMMENT '學號',
  name VARCHAR(60) NOT NULL COMMENT '姓名',
  PRIMARY KEY (id)
) ENGINE=InnoDB COMMENT='學生';

CREATE TABLE course (
  id      INT NOT NULL AUTO_INCREMENT COMMENT '課程編號',
  title   VARCHAR(100) NOT NULL COMMENT '課程名稱',
  credits TINYINT NOT NULL DEFAULT 2 COMMENT '學分數',
  PRIMARY KEY (id)
) ENGINE=InnoDB COMMENT='課程';

CREATE TABLE enrollment (
  student_id  INT NOT NULL COMMENT '學生',
  course_id   INT NOT NULL COMMENT '課程',
  enrolled_on DATE NOT NULL COMMENT '選課日期',
  grade       DECIMAL(5,2) NULL COMMENT '成績',
  PRIMARY KEY (student_id, course_id),
  KEY idx_enrollment_course (course_id),
  CONSTRAINT fk_enrollment_student FOREIGN KEY (student_id) REFERENCES student (id) ON DELETE CASCADE,
  CONSTRAINT fk_enrollment_course  FOREIGN KEY (course_id)  REFERENCES course (id)  ON DELETE CASCADE
) ENGINE=InnoDB COMMENT='選課（學生與課程的多對多）';

CREATE TABLE app_setting (
  setting_key   VARCHAR(60)  NOT NULL COMMENT '設定鍵',
  setting_value VARCHAR(255) NULL COMMENT '設定值',
  PRIMARY KEY (setting_key)
) ENGINE=InnoDB COMMENT='系統設定（沒有任何關聯）';

-- ---- sample rows -------------------------------------------------------------------------------
INSERT INTO customer (id, name, email) VALUES
  (1, '王小明', 'ming@example.com'), (2, '陳美玲', 'meiling@example.com'), (3, 'Acme 股份有限公司', 'buy@acme.example');
INSERT INTO address (customer_id, city, street, is_default) VALUES
  (1, '台北市', '忠孝東路四段 1 號', 1), (1, '新北市', '板橋區文化路 22 號', 0), (2, '台中市', '西屯區河南路 8 號', 1), (3, '高雄市', '前鎮區中山三路 100 號', 1);
INSERT INTO employee (id, name, title, manager_id) VALUES
  (1, '林總經理', '總經理', NULL), (2, '張經理', '業務經理', 1), (3, '李業務', '業務專員', 2), (4, '黃業務', '業務專員', 2);
INSERT INTO category (id, name) VALUES (1, '文具'), (2, '電子產品'), (3, '飲料');
INSERT INTO product (id, category_id, sku, name, price) VALUES
  (1, 1, 'PEN-001', '原子筆（藍）', 15.00), (2, 1, 'NB-001', '筆記本 A5', 45.00),
  (3, 2, 'USB-016', 'USB 隨身碟 16GB', 199.00), (4, 3, 'TEA-500', '烏龍茶 500ml', 25.00);
INSERT INTO orders (id, customer_id, sales_rep_id, status, ordered_at) VALUES
  (1, 1, 3, 'shipped', '2026-11-01 10:00:00'), (2, 1, 4, 'paid', '2026-11-05 14:30:00'),
  (3, 2, NULL, 'new', '2026-11-10 09:15:00'), (4, 3, 3, 'shipped', '2026-11-12 16:45:00');
INSERT INTO order_item (order_id, line_no, product_id, qty) VALUES
  (1, 1, 1, 10), (1, 2, 2, 3), (2, 1, 3, 1), (3, 1, 4, 24), (4, 1, 3, 5), (4, 2, 1, 50);
INSERT INTO shipment (order_id, line_no, tracking, shipped_at) VALUES
  (1, 1, 'TW0001', '2026-11-02 08:00:00'), (1, 2, 'TW0002', '2026-11-02 08:00:00'), (4, 1, 'TW0003', '2026-11-13 11:20:00');
INSERT INTO student (id, name) VALUES (1, '周子涵'), (2, '吳佩珊'), (3, 'Alice Chen');
INSERT INTO course (id, title, credits) VALUES (1, '資料庫系統', 3), (2, '作業系統', 3), (3, '體育', 1);
INSERT INTO enrollment (student_id, course_id, enrolled_on, grade) VALUES
  (1, 1, '2026-09-01', 88.5), (1, 2, '2026-09-01', 92.0), (2, 1, '2026-09-02', NULL), (3, 3, '2026-09-03', 75.0);
INSERT INTO app_setting (setting_key, setting_value) VALUES ('site_name', '關聯圖測試店'), ('currency', 'TWD');
